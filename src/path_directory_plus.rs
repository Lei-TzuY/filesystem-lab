use std::{collections::BTreeMap, io};

use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::fsck::check_device;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;

/// One durable child entry with metadata derived from one recovered namespace snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDirectoryEntryMetadata {
    pub name: String,
    pub inode_id: u64,
    pub kind: InodeKind,
    pub logical_blocks: usize,
    pub namespace_references: usize,
}

/// One bounded lexicographic page from recovered directory metadata enumeration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDirectoryMetadataPage {
    pub entries: Vec<PathDirectoryEntryMetadata>,
    /// Exclusive continuation cursor for the next page, present only when more entries remain.
    pub next_after: Option<String>,
}

/// Enumerates immediate directory children with inode metadata in deterministic name order.
///
/// Any older committed WAL is recovered and checkpointed first. A full read-only fsck then proves
/// allocator/inode/namespace agreement before the directory pathname is resolved and the recovered
/// inode and directory tables are joined. Child symbolic links are reported as symlink inodes rather
/// than followed. `namespace_references` is derived from the complete durable directory table, so
/// hard-linked files and symlinks expose their exact persisted reference count for this snapshot.
///
/// Format v5 does not persist byte length, uid/gid, permissions, timestamps, or a stored link-count
/// field. This API deliberately reports only metadata that can be derived truthfully from durable v5
/// state and does not change the on-disk format.
///
/// # Errors
///
/// Propagates recovery/checkpoint, fsck, pathname-resolution, and metadata decode failures. Returns
/// `InvalidInput` when the resolved pathname is not a directory and `InvalidData` when the resolved
/// directory or any child target is missing despite the validated namespace snapshot.
pub fn list_directory_with_metadata_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<Vec<PathDirectoryEntryMetadata>> {
    recover_journal_and_checkpoint(device, *superblock)?;
    check_device(device)?;

    let directory_inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    let inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;
    enumerate_directory_with_metadata(directory_inode_id, &inodes, &entries)
}

/// Returns one bounded page of immediate child metadata in deterministic lexicographic order.
///
/// `after` is an exclusive name cursor: only children whose persisted names compare greater than
/// the cursor are eligible for the returned page. The cursor does not have to name a currently
/// existing child, so callers can continue safely after a previously returned entry is removed
/// between independent calls. Each call nevertheless observes one freshly recovered and fsck-
/// validated snapshot; this API does not promise a multi-call snapshot or mutation-stable view.
///
/// `next_after` is the final returned name only when at least one additional eligible child remains.
/// Passing that value back as `after` advances to the next page without repeating an entry. A page
/// size of zero is rejected rather than producing an ambiguous non-advancing cursor.
///
/// Like [`list_directory_with_metadata_at_path`], this operation follows the directory pathname's
/// symbolic links but does not follow symbolic-link children while enumerating them. It reports only
/// metadata persisted or derivable in filesystem format v5 and does not modify the on-disk format.
///
/// # Errors
///
/// Returns `InvalidInput` when `limit` is zero or when the resolved pathname is not a directory.
/// Recovery/checkpoint, fsck, pathname-resolution, and persisted metadata errors are propagated.
pub fn list_directory_with_metadata_page_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    after: Option<&str>,
    limit: usize,
) -> io::Result<PathDirectoryMetadataPage> {
    if limit == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pathname directory metadata page limit must be greater than zero",
        ));
    }

    recover_journal_and_checkpoint(device, *superblock)?;
    check_device(device)?;

    let directory_inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    let inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;
    let children = enumerate_directory_with_metadata(directory_inode_id, &inodes, &entries)?;
    paginate_directory_metadata(children, after, limit)
}

fn paginate_directory_metadata(
    children: Vec<PathDirectoryEntryMetadata>,
    after: Option<&str>,
    limit: usize,
) -> io::Result<PathDirectoryMetadataPage> {
    if limit == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pathname directory metadata page limit must be greater than zero",
        ));
    }

    let mut eligible = children
        .into_iter()
        .filter(|entry| after.is_none_or(|cursor| entry.name.as_str() > cursor));
    let mut page_entries = Vec::new();
    page_entries.try_reserve_exact(limit).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "pathname directory metadata page limit exceeds staging capacity",
        )
    })?;

    for _ in 0..limit {
        let Some(entry) = eligible.next() else {
            break;
        };
        page_entries.push(entry);
    }

    let has_more = eligible.next().is_some();
    let next_after = if has_more {
        page_entries.last().map(|entry| entry.name.clone())
    } else {
        None
    };

    Ok(PathDirectoryMetadataPage {
        entries: page_entries,
        next_after,
    })
}

fn enumerate_directory_with_metadata(
    directory_inode_id: u64,
    inodes: &[PersistedInode],
    entries: &[crate::directory_codec::PersistedDirectoryEntry],
) -> io::Result<Vec<PathDirectoryEntryMetadata>> {
    let inode_by_id: BTreeMap<u64, &PersistedInode> =
        inodes.iter().map(|inode| (inode.id, inode)).collect();
    let directory = inode_by_id.get(&directory_inode_id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "resolved directory inode is missing",
        )
    })?;
    if directory.kind != InodeKind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pathname directory enumeration target is not a directory",
        ));
    }

    let mut reference_counts = BTreeMap::<u64, usize>::new();
    for entry in entries {
        let count = reference_counts.entry(entry.target).or_default();
        *count = count.checked_add(1).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "namespace reference count overflow",
            )
        })?;
    }

    let mut children = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.parent == directory_inode_id)
    {
        let target = inode_by_id.get(&entry.target).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "directory entry references missing target inode",
            )
        })?;
        let namespace_references =
            reference_counts
                .get(&entry.target)
                .copied()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "directory entry target has no namespace reference",
                    )
                })?;
        children.push(PathDirectoryEntryMetadata {
            name: entry.name.clone(),
            inode_id: entry.target,
            kind: target.kind,
            logical_blocks: target.blocks.len(),
            namespace_references,
        });
    }
    children.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(children)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory_codec::PersistedDirectoryEntry;

    fn inode(id: u64, kind: InodeKind, blocks: &[u64]) -> PersistedInode {
        PersistedInode {
            id,
            kind,
            blocks: blocks.to_vec(),
        }
    }

    fn metadata(name: &str, inode_id: u64) -> PathDirectoryEntryMetadata {
        PathDirectoryEntryMetadata {
            name: name.to_owned(),
            inode_id,
            kind: InodeKind::File,
            logical_blocks: 0,
            namespace_references: 1,
        }
    }

    #[test]
    fn reports_sorted_metadata_and_namespace_reference_counts() {
        let inodes = vec![
            inode(1, InodeKind::Directory, &[]),
            inode(2, InodeKind::File, &[20, 21]),
            inode(3, InodeKind::Directory, &[]),
        ];
        let entries = vec![
            PersistedDirectoryEntry {
                parent: 1,
                target: 3,
                name: "z-dir".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 1,
                target: 2,
                name: "a-file".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 3,
                target: 2,
                name: "alias".to_owned(),
            },
        ];

        assert_eq!(
            enumerate_directory_with_metadata(1, &inodes, &entries).unwrap(),
            vec![
                PathDirectoryEntryMetadata {
                    name: "a-file".to_owned(),
                    inode_id: 2,
                    kind: InodeKind::File,
                    logical_blocks: 2,
                    namespace_references: 2,
                },
                PathDirectoryEntryMetadata {
                    name: "z-dir".to_owned(),
                    inode_id: 3,
                    kind: InodeKind::Directory,
                    logical_blocks: 0,
                    namespace_references: 1,
                },
            ]
        );
    }

    #[test]
    fn paginates_with_exclusive_cursor_and_more_signal() {
        let children = vec![
            metadata("alpha", 2),
            metadata("bravo", 3),
            metadata("charlie", 4),
        ];

        assert_eq!(
            paginate_directory_metadata(children.clone(), None, 2).unwrap(),
            PathDirectoryMetadataPage {
                entries: vec![metadata("alpha", 2), metadata("bravo", 3)],
                next_after: Some("bravo".to_owned()),
            }
        );
        assert_eq!(
            paginate_directory_metadata(children, Some("bravo"), 2).unwrap(),
            PathDirectoryMetadataPage {
                entries: vec![metadata("charlie", 4)],
                next_after: None,
            }
        );
    }

    #[test]
    fn stale_cursor_advances_lexicographically_without_requiring_a_match() {
        let children = vec![metadata("alpha", 2), metadata("charlie", 4)];
        assert_eq!(
            paginate_directory_metadata(children, Some("bravo"), 1).unwrap(),
            PathDirectoryMetadataPage {
                entries: vec![metadata("charlie", 4)],
                next_after: None,
            }
        );
    }

    #[test]
    fn rejects_zero_page_limit() {
        let error = paginate_directory_metadata(Vec::new(), None, 0).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rejects_non_directory_target() {
        let error = enumerate_directory_with_metadata(2, &[inode(2, InodeKind::File, &[])], &[])
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rejects_dangling_child_target() {
        let entries = vec![PersistedDirectoryEntry {
            parent: 1,
            target: 99,
            name: "dangling".to_owned(),
        }];
        let error =
            enumerate_directory_with_metadata(1, &[inode(1, InodeKind::Directory, &[])], &entries)
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
