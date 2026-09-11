use std::io;

use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_directory_rename_overwrite::rename_overwrite_directory_journaled;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;
use crate::rename_overwrite_tx::{
    rename_overwrite_file_journaled, rename_overwrite_linked_file_journaled,
    rename_overwrite_linked_symlink_journaled, rename_overwrite_symlink_journaled,
};

/// Atomically renames one namespace entry over an existing same-kind destination by pathname.
///
/// Any older committed WAL is recovered and checkpointed before source and destination parent
/// resolution. Parent pathnames follow bounded symbolic-link traversal, while final components are
/// never followed. Endpoint inode kinds are read from the recovered format-v5 metadata. Regular
/// files and symbolic links automatically select the singly-linked or multiply-linked destination
/// transaction from the recovered namespace reference count. Directories dispatch to the existing
/// empty-directory replacement transaction. Mixed endpoint kinds are rejected.
///
/// A single terminal slash on either operand is accepted as directory intent; repeated trailing
/// separators remain invalid. Directory intent never follows a final symbolic link and requires both
/// named endpoints themselves to be directories.
///
/// This dispatch does not change the on-disk format. Format v5 continues to derive link counts from
/// directory references rather than a persisted inode field.
///
/// # Errors
/// Returns `InvalidInput` for malformed paths, mixed endpoint kinds, repeated trailing separators,
/// or directory intent applied to non-directory endpoints. Missing entries/inodes and all recovery,
/// checkpoint, pathname resolution, transaction validation, and device I/O failures propagate.
pub fn rename_overwrite_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    let (source, source_directory_intent) =
        normalize_directory_intent(source, "rename-overwrite source")?;
    let (destination, destination_directory_intent) =
        normalize_directory_intent(destination, "rename-overwrite destination")?;
    let (old_parent, old_name, new_parent, new_name) =
        resolve_rename_overwrite_parents(device, superblock, source, destination)?;

    let entries = load_directory_table(device, superblock)?;
    let source_target = entries
        .iter()
        .find(|entry| entry.parent == old_parent && entry.name == old_name)
        .map(|entry| entry.target)
        .ok_or_else(|| invalid_input("rename-overwrite source entry does not exist"))?;
    let destination_target = entries
        .iter()
        .find(|entry| entry.parent == new_parent && entry.name == new_name)
        .map(|entry| entry.target)
        .ok_or_else(|| invalid_input("rename-overwrite destination entry does not exist"))?;

    let inodes = load_inode_table(device, superblock)?;
    let source_kind = inodes
        .iter()
        .find(|inode| inode.id == source_target)
        .map(|inode| inode.kind)
        .ok_or_else(|| invalid_input("rename-overwrite source inode is missing"))?;
    let destination_kind = inodes
        .iter()
        .find(|inode| inode.id == destination_target)
        .map(|inode| inode.kind)
        .ok_or_else(|| invalid_input("rename-overwrite destination inode is missing"))?;

    if source_kind != destination_kind {
        return Err(invalid_input(
            "rename-overwrite source and destination must have matching inode kinds",
        ));
    }
    if (source_directory_intent || destination_directory_intent)
        && source_kind != InodeKind::Directory
    {
        return Err(invalid_input(
            "trailing-slash rename-overwrite requires directory endpoints",
        ));
    }

    let destination_references = entries
        .iter()
        .filter(|entry| entry.target == destination_target)
        .count();

    match source_kind {
        InodeKind::File if destination_references == 1 => rename_overwrite_file_journaled(
            device, superblock, old_parent, old_name, new_parent, new_name,
        ),
        InodeKind::File => rename_overwrite_linked_file_journaled(
            device, superblock, old_parent, old_name, new_parent, new_name,
        ),
        InodeKind::Symlink if destination_references == 1 => rename_overwrite_symlink_journaled(
            device, superblock, old_parent, old_name, new_parent, new_name,
        ),
        InodeKind::Symlink => rename_overwrite_linked_symlink_journaled(
            device, superblock, old_parent, old_name, new_parent, new_name,
        ),
        InodeKind::Directory => rename_overwrite_directory_journaled(
            device, superblock, old_parent, old_name, new_parent, new_name,
        ),
    }
}

/// Atomically renames one regular file over an existing singly linked regular file by pathname.
///
/// Any older committed WAL is recovered and checkpointed before source and destination parent
/// pathname resolution so endpoint selection is derived from recovered namespace state. Source and
/// destination parent pathnames then follow the repository-wide bounded symbolic-link rules.
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
    let (old_parent, old_name, new_parent, new_name) =
        resolve_rename_overwrite_parents(device, superblock, source, destination)?;
    rename_overwrite_file_journaled(
        device, superblock, old_parent, old_name, new_parent, new_name,
    )
}

/// Atomically renames one regular file over one alias of a multiply linked regular file by pathname.
///
/// Any older committed WAL is recovered and checkpointed before parent pathname resolution. Only
/// the selected destination namespace entry is replaced; the destination inode and its other aliases
/// remain alive. Final components are never followed.
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
    let (old_parent, old_name, new_parent, new_name) =
        resolve_rename_overwrite_parents(device, superblock, source, destination)?;
    rename_overwrite_linked_file_journaled(
        device, superblock, old_parent, old_name, new_parent, new_name,
    )
}

/// Atomically renames one symbolic link over an existing singly linked symbolic link by pathname.
///
/// Any older committed WAL is recovered and checkpointed before parent pathname resolution. Only
/// parent pathnames follow bounded symbolic-link traversal; final components are deliberately not
/// followed, so the link inodes themselves participate in replacement even when their targets are
/// dangling. The destination symlink inode and its payload block are released atomically with
/// namespace publication while the source inode and payload survive under the destination name.
///
/// # Errors
/// Returns `InvalidInput` for malformed paths, non-symlink endpoints, same-inode aliases, or a
/// multiply linked destination. Parent-resolution, ownership, WAL, recovery, checkpoint, and device
/// I/O failures are propagated.
pub fn rename_overwrite_symlink_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    let (old_parent, old_name, new_parent, new_name) =
        resolve_rename_overwrite_parents(device, superblock, source, destination)?;
    rename_overwrite_symlink_journaled(
        device, superblock, old_parent, old_name, new_parent, new_name,
    )
}

/// Atomically renames one symbolic link over one alias of a multiply linked symbolic link.
///
/// Any older committed WAL is recovered and checkpointed before parent pathname resolution. Parent
/// pathnames then follow bounded symbolic-link traversal while final components are never followed.
/// Only the selected destination alias is replaced; the destination symlink inode, payload block,
/// allocator ownership, and every other alias remain alive.
///
/// # Errors
/// Returns `InvalidInput` for malformed paths, non-symlink endpoints, same-inode aliases, or a
/// destination with fewer than two namespace references. Parent-resolution, WAL, recovery,
/// checkpoint, and device I/O failures are propagated.
pub fn rename_overwrite_linked_symlink_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    let (old_parent, old_name, new_parent, new_name) =
        resolve_rename_overwrite_parents(device, superblock, source, destination)?;
    rename_overwrite_linked_symlink_journaled(
        device, superblock, old_parent, old_name, new_parent, new_name,
    )
}

fn resolve_rename_overwrite_parents<'a>(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &'a str,
    destination: &'a str,
) -> io::Result<(u64, &'a str, u64, &'a str)> {
    let (old_parent_path, old_name) = split_path(source, "rename-overwrite source")?;
    let (new_parent_path, new_name) = split_path(destination, "rename-overwrite destination")?;
    recover_journal_and_checkpoint(device, *superblock)?;
    let old_parent = resolve_path_following_symlinks(device, superblock, old_parent_path)?;
    let new_parent = resolve_path_following_symlinks(device, superblock, new_parent_path)?;
    Ok((old_parent, old_name, new_parent, new_name))
}

fn normalize_directory_intent<'a>(path: &'a str, label: &str) -> io::Result<(&'a str, bool)> {
    if path == "/" || !path.ends_with('/') {
        return Ok((path, false));
    }
    let stripped = &path[..path.len() - 1];
    if stripped.ends_with('/') {
        return Err(invalid_input(format!(
            "{label} contains repeated trailing separators"
        )));
    }
    Ok((stripped, true))
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
