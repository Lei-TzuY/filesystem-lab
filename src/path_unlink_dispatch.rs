use std::io;

use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::hard_unlink_tx::{
    unlink_nonfinal_file_link_journaled, unlink_nonfinal_symlink_link_journaled,
};
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::path_unlink::unlink_file_journaled;
use crate::recovery::RecoveryReport;
use crate::symlink_unlink::unlink_symlink_journaled;

/// Removes one non-directory namespace entry using POSIX-like `unlink` dispatch semantics.
///
/// Any older committed WAL is recovered and checkpointed before parent-path resolution. Intermediate
/// symbolic links are followed, but the final component is deliberately treated as a namespace key
/// rather than followed. The persisted target inode kind and its current directory-entry reference
/// count select the existing durable lifecycle:
///
/// - multiply referenced regular files use the directory-only non-final hard-link unlink;
/// - singly referenced regular files use final unlink and release all owned data blocks;
/// - multiply referenced symbolic links use the directory-only non-final symlink unlink;
/// - singly referenced symbolic links use final symlink unlink and release payload blocks;
/// - directories are rejected and remain owned by the explicit `rmdir`-style surface.
///
/// Format v5 stores no link-count field, so the authoritative count is derived from the recovered
/// directory table immediately before dispatch. No on-disk format change is required.
///
/// # Errors
/// Returns `InvalidInput` for non-absolute/root/trailing-slash paths, missing final namespace
/// entries or target inodes, and directory targets. Parent-resolution, recovery/checkpoint,
/// corruption validation, WAL, and durable I/O errors from the selected lifecycle propagate.
pub fn unlink_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<RecoveryReport> {
    let (parent_path, name) = split_path(path)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;

    let entries = load_directory_table(device, superblock)?;
    let entry = entries
        .iter()
        .find(|entry| entry.parent == parent && entry.name == name)
        .ok_or_else(|| invalid_input("unlink entry is missing"))?;
    let target = entry.target;
    let reference_count = entries
        .iter()
        .filter(|entry| entry.target == target)
        .count();

    let target_kind = load_inode_table(device, superblock)?
        .iter()
        .find(|inode| inode.id == target)
        .map(|inode| inode.kind)
        .ok_or_else(|| invalid_input("unlink target inode is missing"))?;

    match (target_kind, reference_count) {
        (InodeKind::File | InodeKind::Symlink, 0) => {
            Err(invalid_input("unlink target has no namespace references"))
        }
        (InodeKind::File, 1) => unlink_file_journaled(device, superblock, parent, name),
        (InodeKind::File, _) => {
            unlink_nonfinal_file_link_journaled(device, superblock, parent, name)
        }
        (InodeKind::Symlink, 1) => unlink_symlink_journaled(device, superblock, parent, name),
        (InodeKind::Symlink, _) => {
            unlink_nonfinal_symlink_link_journaled(device, superblock, parent, name)
        }
        (InodeKind::Directory, _) => Err(invalid_input(
            "unlink cannot remove a directory; use directory removal",
        )),
    }
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
