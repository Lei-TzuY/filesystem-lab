use std::collections::BTreeSet;
use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;
use crate::unlink_tx::store_unlink_metadata_journaled;

/// Removes the final durable namespace reference to a regular file.
///
/// The selected entry must target a regular-file inode with exactly one namespace reference. Every
/// referenced data block must be uniquely listed by that inode and allocator-owned. The operation
/// frees exactly those blocks, removes the inode and namespace entry, and publishes allocation,
/// inode, and directory metadata through the existing bounded unlink WAL transaction.
///
/// # Errors
/// Returns `InvalidInput` when the selected entry is missing, targets a non-file inode, has another
/// namespace reference, contains duplicate block references, or references a block not owned by the
/// allocator. Durable metadata, WAL, recovery, checkpoint, and device errors are propagated.
pub fn unlink_file_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
) -> io::Result<RecoveryReport> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;

    let entry_index = entries
        .iter()
        .position(|entry| entry.parent == parent && entry.name == name)
        .ok_or_else(|| invalid_input("regular-file entry is missing"))?;
    let target = entries[entry_index].target;
    let inode_index = inodes
        .iter()
        .position(|inode| inode.id == target)
        .ok_or_else(|| invalid_input("regular-file target inode is missing"))?;
    let inode = &inodes[inode_index];

    if inode.kind != InodeKind::File {
        return Err(invalid_input("unlink requires a regular-file inode"));
    }
    if entries.iter().filter(|entry| entry.target == target).count() != 1 {
        return Err(invalid_input(
            "regular-file final unlink requires exactly one namespace reference",
        ));
    }

    let unique_blocks: BTreeSet<u64> = inode.blocks.iter().copied().collect();
    if unique_blocks.len() != inode.blocks.len() {
        return Err(invalid_input(
            "regular-file inode contains duplicate block references",
        ));
    }
    for block in &inode.blocks {
        if !allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
        {
            return Err(invalid_input(
                "regular-file data block is not allocator-owned",
            ));
        }
    }

    for block in &inode.blocks {
        allocator
            .free(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    }
    inodes.remove(inode_index);
    entries.remove(entry_index);

    store_unlink_metadata_journaled(device, superblock, &allocator, &inodes, &entries)
}

/// Removes a singly referenced regular file addressed by an absolute pathname.
///
/// Intermediate components, including the parent itself, use the repository-wide bounded symlink
/// expansion rules. The final component is intentionally not resolved: a final symlink is rejected
/// as a non-file inode instead of deleting its target. After pathname-shape validation and before
/// resolving the parent, any older durable journal is recovered and checkpointed so the unlink is
/// recomputed from recovered home state rather than a partial post-crash home-write prefix.
/// Publication is delegated to [`unlink_file_journaled`], preserving one
/// allocation/inode/directory WAL transaction.
///
/// # Errors
/// Returns `InvalidInput` when the pathname is not absolute, names the root, has an empty final
/// component, or names an unsupported target. Parent-resolution errors and all recovery,
/// checkpoint, [`unlink_file_journaled`] durable I/O errors are propagated.
pub fn unlink_file_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<RecoveryReport> {
    let (parent_path, name) = split_path(path)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;
    unlink_file_journaled(device, superblock, parent, name)
}

/// Removes one empty directory from a durable namespace.
///
/// The selected entry must target a non-root directory inode with exactly one namespace reference
/// and no child entries. Any blocks listed by the inode must be unique and allocator-owned; the
/// operation frees exactly those blocks before removing the inode and namespace entry. Publication
/// uses the existing validated unlink WAL transaction, so allocator, inode, and directory metadata
/// advance atomically.
///
/// # Errors
/// Returns `InvalidInput` when the selected entry is missing, targets a non-directory inode, names
/// the root inode, has another namespace reference, is non-empty, contains duplicate block
/// references, or references a block not owned by the allocator. Durable metadata, WAL, recovery,
/// checkpoint, and device errors are propagated.
pub fn remove_directory_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
) -> io::Result<RecoveryReport> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;

    let entry_index = entries
        .iter()
        .position(|entry| entry.parent == parent && entry.name == name)
        .ok_or_else(|| invalid_input("directory entry is missing"))?;
    let target = entries[entry_index].target;
    let inode_index = inodes
        .iter()
        .position(|inode| inode.id == target)
        .ok_or_else(|| invalid_input("directory target inode is missing"))?;
    let inode = &inodes[inode_index];

    if target == 1 {
        return Err(invalid_input(
            "directory removal cannot remove the root inode",
        ));
    }
    if inode.kind != InodeKind::Directory {
        return Err(invalid_input(
            "directory removal requires a directory inode",
        ));
    }
    if entries.iter().filter(|entry| entry.target == target).count() != 1 {
        return Err(invalid_input(
            "directory removal requires exactly one namespace reference",
        ));
    }
    if entries.iter().any(|entry| entry.parent == target) {
        return Err(invalid_input(
            "directory removal requires an empty directory",
        ));
    }

    let unique_blocks: BTreeSet<u64> = inode.blocks.iter().copied().collect();
    if unique_blocks.len() != inode.blocks.len() {
        return Err(invalid_input(
            "directory inode contains duplicate block references",
        ));
    }
    for block in &inode.blocks {
        if !allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
        {
            return Err(invalid_input("directory data block is not allocator-owned"));
        }
    }

    for block in &inode.blocks {
        allocator
            .free(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    }
    inodes.remove(inode_index);
    entries.remove(entry_index);

    store_unlink_metadata_journaled(device, superblock, &allocator, &inodes, &entries)
}

/// Removes an empty directory addressed by an absolute pathname.
///
/// Intermediate components, including the parent itself, use bounded symlink expansion. The final
/// component is intentionally not resolved, matching `rmdir`-style semantics: a final symlink is
/// rejected instead of removing the directory it targets. After pathname-shape validation and
/// before resolving the parent, any older durable journal is recovered and checkpointed so the
/// removal is recomputed from recovered home state rather than a partial post-crash home prefix.
///
/// # Errors
/// Returns `InvalidInput` when the pathname is not absolute, names the root, has an empty final
/// component, or names an unsupported/non-empty target. Parent-resolution errors and all recovery,
/// checkpoint, and [`remove_directory_journaled`] durable I/O errors are propagated.
pub fn remove_directory_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<RecoveryReport> {
    let (parent_path, name) = split_path(path)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;
    remove_directory_journaled(device, superblock, parent, name)
}

fn split_path(path: &str) -> io::Result<(&str, &str)> {
    if !path.starts_with('/') {
        return Err(invalid_input("unlink path must be an absolute path"));
    }
    if path == "/" {
        return Err(invalid_input("unlink path cannot be the root path"));
    }

    let (parent, name) = path
        .rsplit_once('/')
        .ok_or_else(|| invalid_input("unlink path must contain a final component"))?;
    if name.is_empty() {
        return Err(invalid_input("unlink path final component is empty"));
    }

    Ok((if parent.is_empty() { "/" } else { parent }, name))
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_preserves_root_and_nested_parent_paths() {
        assert_eq!(split_path("/file").unwrap(), ("/", "file"));
        assert_eq!(split_path("/dir/sub/file").unwrap(), ("/dir/sub", "file"));
    }

    #[test]
    fn split_rejects_non_absolute_root_and_trailing_slash() {
        for path in ["file", "/", "/dir/"] {
            assert_eq!(
                split_path(path).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
