use std::{collections::BTreeMap, io};

use crate::block::BlockDevice;
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;

/// One durable child entry returned by [`read_dir_at_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDirectoryEntry {
    pub name: String,
    pub inode_id: u64,
    pub kind: InodeKind,
}

/// Enumerates the direct children of a directory pathname in deterministic name order.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution, so the
/// selected directory and the namespace snapshot come from recovered durable state. Intermediate
/// and final symbolic links are followed with the repository-wide bounded expansion rules. The
/// returned vector contains only persisted child entries; synthetic `.` and `..` entries are not
/// invented because format v5 does not store them.
///
/// Each child target must exist in the inode table. This keeps namespace enumeration aligned with
/// fsck's dangling-target invariant instead of silently omitting corrupt entries. No on-disk state
/// is modified and the format remains v5.
///
/// # Errors
///
/// Propagates recovery/checkpoint, pathname resolution, inode-table, and directory-table errors.
/// Returns `InvalidInput` when the resolved pathname is not a directory and `InvalidData` when the
/// resolved inode or any enumerated child target is missing from the persisted inode table.
pub fn read_dir_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<Vec<PathDirectoryEntry>> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let directory_inode = resolve_path_following_symlinks(device, superblock, path)?;
    let inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;
    enumerate_directory(directory_inode, &inodes, &entries)
}

fn enumerate_directory(
    directory_inode: u64,
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
) -> io::Result<Vec<PathDirectoryEntry>> {
    let inode_by_id: BTreeMap<u64, &PersistedInode> =
        inodes.iter().map(|inode| (inode.id, inode)).collect();
    let directory = inode_by_id
        .get(&directory_inode)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "resolved directory inode is missing"))?;
    if directory.kind != InodeKind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pathname directory enumeration target is not a directory",
        ));
    }

    let mut children = Vec::new();
    for entry in entries.iter().filter(|entry| entry.parent == directory_inode) {
        let target = inode_by_id.get(&entry.target).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "directory entry references missing target inode",
            )
        })?;
        children.push(PathDirectoryEntry {
            name: entry.name.clone(),
            inode_id: entry.target,
            kind: target.kind,
        });
    }
    children.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(children)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inode(id: u64, kind: InodeKind) -> PersistedInode {
        PersistedInode {
            id,
            kind,
            blocks: Vec::new(),
        }
    }

    #[test]
    fn enumerates_children_in_name_order_with_kinds() {
        let inodes = vec![
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::File),
            inode(3, InodeKind::Directory),
            inode(4, InodeKind::Symlink),
        ];
        let entries = vec![
            PersistedDirectoryEntry {
                parent: 1,
                target: 4,
                name: "z-link".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 1,
                target: 2,
                name: "a-file".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 1,
                target: 3,
                name: "m-dir".to_owned(),
            },
        ];

        assert_eq!(
            enumerate_directory(1, &inodes, &entries).unwrap(),
            vec![
                PathDirectoryEntry {
                    name: "a-file".to_owned(),
                    inode_id: 2,
                    kind: InodeKind::File,
                },
                PathDirectoryEntry {
                    name: "m-dir".to_owned(),
                    inode_id: 3,
                    kind: InodeKind::Directory,
                },
                PathDirectoryEntry {
                    name: "z-link".to_owned(),
                    inode_id: 4,
                    kind: InodeKind::Symlink,
                },
            ]
        );
    }

    #[test]
    fn empty_directory_returns_empty_listing() {
        let inodes = vec![inode(1, InodeKind::Directory)];
        assert!(enumerate_directory(1, &inodes, &[]).unwrap().is_empty());
    }

    #[test]
    fn rejects_non_directory_target() {
        let error = enumerate_directory(2, &[inode(2, InodeKind::File)], &[]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rejects_dangling_child_target() {
        let entries = vec![PersistedDirectoryEntry {
            parent: 1,
            target: 99,
            name: "dangling".to_owned(),
        }];
        let error = enumerate_directory(1, &[inode(1, InodeKind::Directory)], &entries).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
