use std::collections::HashSet;
use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_codec::{encode_directory_entry, PersistedDirectoryEntry};
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::load_inode_table;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Creates one durable zero-block regular file at an absolute pathname.
///
/// The destination parent is resolved with the existing bounded symbolic-link rules. The final
/// component is not resolved: it becomes one new durable directory entry naming a freshly assigned
/// regular-file inode. Format v5 has no persisted byte length, so the created file starts with an
/// empty logical-block vector and therefore owns no data blocks.
///
/// The inode-table and directory-table changes are published together through the existing create
/// WAL transaction. The allocator image is passed through unchanged, which makes zero-block create
/// preserve exact allocated/free accounting.
///
/// # Errors
///
/// Returns `InvalidInput` for malformed destinations, a non-directory parent, namespace collision,
/// invalid directory-entry name, or exhausted inode identifiers. Returns `InvalidData` when the
/// persisted inode table contains duplicate identifiers. Parent-resolution, metadata decoding, WAL,
/// recovery, checkpoint, and device I/O errors are propagated.
pub fn create_empty_file_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    destination: &str,
) -> io::Result<(u64, RecoveryReport)> {
    let (parent_path, name) = split_destination(destination)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;

    let allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;

    let parent_inode = inodes
        .iter()
        .find(|inode| inode.id == parent)
        .ok_or_else(|| invalid_data("resolved parent inode is missing from inode table"))?;
    if parent_inode.kind != InodeKind::Directory {
        return Err(invalid_input("create parent pathname is not a directory"));
    }
    if entries
        .iter()
        .any(|entry| entry.parent == parent && entry.name == name)
    {
        return Err(invalid_input("create destination already exists"));
    }

    let mut seen = HashSet::with_capacity(inodes.len());
    let mut max_inode_id = 0_u64;
    for inode in &inodes {
        if !seen.insert(inode.id) {
            return Err(invalid_data("inode table contains duplicate identifiers"));
        }
        max_inode_id = max_inode_id.max(inode.id);
    }
    let inode_id = max_inode_id
        .checked_add(1)
        .filter(|id| *id != 0)
        .ok_or_else(|| invalid_input("no fresh inode identifier is available"))?;

    let new_entry = PersistedDirectoryEntry {
        parent,
        target: inode_id,
        name: name.to_owned(),
    };
    encode_directory_entry(&new_entry)?;

    inodes.push(PersistedInode {
        id: inode_id,
        kind: InodeKind::File,
        blocks: Vec::new(),
    });
    entries.push(new_entry);

    let report = store_create_metadata_journaled(
        device,
        superblock,
        &allocator,
        &inodes,
        &entries,
    )?;
    Ok((inode_id, report))
}

fn split_destination(path: &str) -> io::Result<(&str, &str)> {
    if !path.starts_with('/') {
        return Err(invalid_input("create destination must be an absolute path"));
    }
    if path == "/" {
        return Err(invalid_input("create destination cannot be the root path"));
    }

    let (parent, name) = path
        .rsplit_once('/')
        .ok_or_else(|| invalid_input("create destination must contain a final component"))?;
    if name.is_empty() {
        return Err(invalid_input("create destination final component is empty"));
    }

    Ok((if parent.is_empty() { "/" } else { parent }, name))
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_split_preserves_root_and_nested_parent_paths() {
        assert_eq!(split_destination("/file").unwrap(), ("/", "file"));
        assert_eq!(
            split_destination("/dir/sub/file").unwrap(),
            ("/dir/sub", "file")
        );
    }

    #[test]
    fn destination_split_rejects_non_absolute_root_and_trailing_slash() {
        for path in ["file", "/", "/dir/"] {
            assert_eq!(
                split_destination(path).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
