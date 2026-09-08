use std::io;

use crate::block::BlockDevice;
use crate::format::Superblock;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;
use crate::rename_overwrite_tx::{
    rename_overwrite_file_journaled, rename_overwrite_linked_file_journaled,
};

/// Atomically renames one regular file over an existing singly linked regular file by pathname.
///
/// Source and destination parent pathnames follow the repository-wide bounded symbolic-link rules.
/// Neither final component is followed. The replaced destination inode and its data ownership are
/// released atomically with namespace publication by the existing rename-overwrite WAL transaction.
///
/// # Errors
/// Returns `InvalidInput` for malformed absolute paths and propagates parent-resolution,
/// rename-overwrite validation, recovery, checkpoint, and device I/O failures.
pub fn rename_overwrite_file_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    rename_overwrite_at_path_impl(device, superblock, source, destination, false)
}

/// Atomically renames one regular file over one alias of a multiply linked regular file by pathname.
///
/// Only the selected destination namespace entry is replaced; the destination inode and its other
/// aliases remain alive. Final components are never followed.
///
/// # Errors
/// Returns `InvalidInput` for malformed absolute paths and propagates parent-resolution,
/// linked-rename-overwrite validation, recovery, checkpoint, and device I/O failures.
pub fn rename_overwrite_linked_file_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    rename_overwrite_at_path_impl(device, superblock, source, destination, true)
}

fn rename_overwrite_at_path_impl(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
    linked_destination: bool,
) -> io::Result<RecoveryReport> {
    let (old_parent_path, old_name) = split_path(source, "rename-overwrite source")?;
    let (new_parent_path, new_name) = split_path(destination, "rename-overwrite destination")?;
    let old_parent = resolve_path_following_symlinks(device, superblock, old_parent_path)?;
    let new_parent = resolve_path_following_symlinks(device, superblock, new_parent_path)?;

    if linked_destination {
        rename_overwrite_linked_file_journaled(
            device, superblock, old_parent, old_name, new_parent, new_name,
        )
    } else {
        rename_overwrite_file_journaled(
            device, superblock, old_parent, old_name, new_parent, new_name,
        )
    }
}

fn split_path<'a>(path: &'a str, label: &str) -> io::Result<(&'a str, &'a str)> {
    if !path.starts_with('/') {
        return Err(invalid_input(format!("{label} must be an absolute path")));
    }
    if path == "/" {
        return Err(invalid_input(format!("{label} cannot be the root path")));
    }
    let (parent, name) = path
        .rsplit_once('/')
        .ok_or_else(|| invalid_input(format!("{label} must contain a final component")))?;
    if name.is_empty() {
        return Err(invalid_input(format!("{label} final component is empty")));
    }
    Ok((if parent.is_empty() { "/" } else { parent }, name))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
