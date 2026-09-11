use std::io;

use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;
use crate::rename_exchange_tx::{
    rename_exchange_directories_journaled, rename_exchange_files_journaled,
    rename_exchange_symlinks_journaled,
};

/// Atomically exchanges two existing same-kind namespace entries addressed by absolute pathnames.
///
/// Any older committed journal image is recovered and checkpointed before either parent pathname is
/// resolved. Parent pathnames follow bounded symbolic-link traversal, while final components are
/// never followed. The recovered format-v5 inode kinds then select the existing regular-file,
/// symbolic-link, or directory exchange transaction.
///
/// A single terminal slash on either operand expresses directory intent. Repeated trailing
/// separators remain invalid, and directory intent requires both named final entries themselves to
/// be directories; it never follows a final symbolic link merely because a slash was present.
/// Mixed endpoint kinds are rejected before publication.
///
/// This dispatch does not change the on-disk format: exchange remains a directory-only WAL mutation
/// and preserves inode/data ownership for all three supported inode kinds.
///
/// # Errors
/// Returns `InvalidInput` for malformed paths, mixed endpoint kinds, repeated trailing separators,
/// or directory intent applied to non-directory endpoints. Missing entries/inodes and all recovery,
/// checkpoint, pathname-resolution, transaction-validation, and device-I/O errors propagate.
pub fn rename_exchange_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first: &str,
    second: &str,
) -> io::Result<RecoveryReport> {
    let (first, first_directory_intent) =
        normalize_directory_intent(first, "first rename-exchange path")?;
    let (second, second_directory_intent) =
        normalize_directory_intent(second, "second rename-exchange path")?;
    let (first_parent_path, first_name) = split_path(first, "first rename-exchange path")?;
    let (second_parent_path, second_name) = split_path(second, "second rename-exchange path")?;

    recover_journal_and_checkpoint(device, *superblock)?;
    let first_parent = resolve_path_following_symlinks(device, superblock, first_parent_path)?;
    let second_parent = resolve_path_following_symlinks(device, superblock, second_parent_path)?;

    let entries = load_directory_table(device, superblock)?;
    let first_target = entries
        .iter()
        .find(|entry| entry.parent == first_parent && entry.name == first_name)
        .map(|entry| entry.target)
        .ok_or_else(|| invalid_input("first rename-exchange entry does not exist"))?;
    let second_target = entries
        .iter()
        .find(|entry| entry.parent == second_parent && entry.name == second_name)
        .map(|entry| entry.target)
        .ok_or_else(|| invalid_input("second rename-exchange entry does not exist"))?;

    let inodes = load_inode_table(device, superblock)?;
    let first_kind = inodes
        .iter()
        .find(|inode| inode.id == first_target)
        .map(|inode| inode.kind)
        .ok_or_else(|| invalid_input("first rename-exchange inode is missing"))?;
    let second_kind = inodes
        .iter()
        .find(|inode| inode.id == second_target)
        .map(|inode| inode.kind)
        .ok_or_else(|| invalid_input("second rename-exchange inode is missing"))?;

    if first_kind != second_kind {
        return Err(invalid_input(
            "rename-exchange endpoints must have matching inode kinds",
        ));
    }
    if (first_directory_intent || second_directory_intent) && first_kind != InodeKind::Directory {
        return Err(invalid_input(
            "trailing-slash rename-exchange requires directory endpoints",
        ));
    }

    match first_kind {
        InodeKind::File => rename_exchange_files_journaled(
            device,
            superblock,
            first_parent,
            first_name,
            second_parent,
            second_name,
        ),
        InodeKind::Symlink => rename_exchange_symlinks_journaled(
            device,
            superblock,
            first_parent,
            first_name,
            second_parent,
            second_name,
        ),
        InodeKind::Directory => rename_exchange_directories_journaled(
            device,
            superblock,
            first_parent,
            first_name,
            second_parent,
            second_name,
        ),
    }
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
