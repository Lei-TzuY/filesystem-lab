use std::io;

use crate::block::BlockDevice;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;
use crate::rename_exchange_tx::{
    rename_exchange_directories_journaled, rename_exchange_files_journaled,
    rename_exchange_symlinks_journaled,
};
use crate::rename_tx::rename_entry_journaled;

/// Atomically renames one durable namespace entry addressed by absolute pathnames.
///
/// Only the source and destination parent pathnames are resolved through the repository-wide
/// bounded symbolic-link traversal rules. Neither final component is followed, so renaming a
/// symbolic link moves the link inode itself and an existing destination remains a collision.
///
/// After validating both pathname shapes but before resolving either parent, the operation recovers
/// and checkpoints any older durable journal image. The rename is therefore derived only from fully
/// recovered namespace state rather than from a partial post-crash home-write prefix. Publication is
/// delegated to [`rename_entry_journaled`], preserving its directory-only WAL transaction and its
/// directory-cycle validation.
///
/// # Errors
/// Returns `InvalidInput` when either pathname is not absolute, names the root, or has an empty
/// final component. Parent-resolution errors and all recovery, checkpoint,
/// [`rename_entry_journaled`] validation, and durable I/O errors are propagated.
pub fn rename_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    let (old_parent_path, old_name) = split_path(source, "rename source")?;
    let (new_parent_path, new_name) = split_path(destination, "rename destination")?;

    recover_journal_and_checkpoint(device, *superblock)?;
    let old_parent = resolve_path_following_symlinks(device, superblock, old_parent_path)?;
    let new_parent = resolve_path_following_symlinks(device, superblock, new_parent_path)?;

    rename_entry_journaled(
        device, superblock, old_parent, old_name, new_parent, new_name,
    )
}

/// Atomically exchanges two existing regular-file namespace entries addressed by pathnames.
///
/// Only the parent portions are resolved through bounded symbolic-link traversal. Final components
/// are deliberately not followed, so both namespace keys are exchanged exactly as named. Any older
/// durable WAL is recovered and checkpointed after pathname-shape validation but before parent
/// resolution. The durable mutation is delegated to [`rename_exchange_files_journaled`], preserving
/// its directory-only WAL transaction, regular-file validation, hard-link-alias no-op semantics, and
/// pre-publication fsck check.
///
/// # Errors
/// Returns `InvalidInput` when either pathname is not absolute, names the root, or has an empty
/// final component. Parent-resolution errors and all recovery, checkpoint,
/// [`rename_exchange_files_journaled`] validation, and durable I/O errors are propagated.
pub fn rename_exchange_files_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first: &str,
    second: &str,
) -> io::Result<RecoveryReport> {
    let (first_parent, first_name, second_parent, second_name) =
        resolve_exchange_parents(device, superblock, first, second)?;

    rename_exchange_files_journaled(
        device,
        superblock,
        first_parent,
        first_name,
        second_parent,
        second_name,
    )
}

/// Atomically exchanges two existing symbolic-link namespace entries addressed by pathnames.
///
/// Parent portions follow bounded symbolic-link traversal while final components remain unfollowed,
/// so the symbolic-link inodes themselves are exchanged even when either persisted target is
/// dangling. Any older durable WAL is recovered and checkpointed before parent resolution.
/// Publication is delegated to [`rename_exchange_symlinks_journaled`], which advances only the
/// directory table and preserves both link inodes, payload blocks, and allocator ownership.
///
/// # Errors
/// Returns `InvalidInput` when either pathname is not absolute, names the root, has an empty final
/// component, or either final namespace target is not a symbolic link. Parent-resolution errors and
/// all recovery, checkpoint, [`rename_exchange_symlinks_journaled`] validation, and durable I/O
/// errors are propagated.
pub fn rename_exchange_symlinks_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first: &str,
    second: &str,
) -> io::Result<RecoveryReport> {
    let (first_parent, first_name, second_parent, second_name) =
        resolve_exchange_parents(device, superblock, first, second)?;

    rename_exchange_symlinks_journaled(
        device,
        superblock,
        first_parent,
        first_name,
        second_parent,
        second_name,
    )
}

/// Atomically exchanges two existing directory namespace entries addressed by pathnames.
///
/// Parent portions follow bounded symbolic-link traversal while final components remain unfollowed.
/// Any older durable WAL is recovered and checkpointed before parent resolution. Publication is
/// delegated to [`rename_exchange_directories_journaled`], which preserves inode and allocator
/// images and rejects any candidate namespace that would introduce a directory cycle.
///
/// # Errors
/// Returns `InvalidInput` when either pathname is not absolute, names the root, has an empty final
/// component, either final target is not a directory, or the exchange would create a directory
/// cycle. Parent-resolution errors and all recovery, checkpoint,
/// [`rename_exchange_directories_journaled`] durable I/O errors are propagated.
pub fn rename_exchange_directories_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first: &str,
    second: &str,
) -> io::Result<RecoveryReport> {
    let (first_parent, first_name, second_parent, second_name) =
        resolve_exchange_parents(device, superblock, first, second)?;

    rename_exchange_directories_journaled(
        device,
        superblock,
        first_parent,
        first_name,
        second_parent,
        second_name,
    )
}

fn resolve_exchange_parents<'a>(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first: &'a str,
    second: &'a str,
) -> io::Result<(u64, &'a str, u64, &'a str)> {
    let (first_parent_path, first_name) = split_path(first, "first exchange path")?;
    let (second_parent_path, second_name) = split_path(second, "second exchange path")?;

    recover_journal_and_checkpoint(device, *superblock)?;
    let first_parent = resolve_path_following_symlinks(device, superblock, first_parent_path)?;
    let second_parent = resolve_path_following_symlinks(device, superblock, second_parent_path)?;
    Ok((first_parent, first_name, second_parent, second_name))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_preserves_root_and_nested_parent_paths() {
        assert_eq!(split_path("/file", "source").unwrap(), ("/", "file"));
        assert_eq!(
            split_path("/dir/sub/file", "source").unwrap(),
            ("/dir/sub", "file")
        );
    }

    #[test]
    fn split_rejects_non_absolute_root_and_trailing_slash() {
        for path in ["file", "/", "/dir/"] {
            assert_eq!(
                split_path(path, "source").unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
