use std::io;

use crate::block::BlockDevice;
use crate::format::Superblock;
use crate::hard_link_tx::{hard_link_file_journaled, hard_link_symlink_journaled};
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::{
    resolve_path_following_symlinks, resolve_path_without_following_final_symlink,
};
use crate::recovery::RecoveryReport;

/// Selects whether pathname hard-link creation follows a symbolic link in the final source
/// component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HardLinkSourceFollow {
    /// POSIX `link`-like behavior: intermediate source symlinks are followed, but the final source
    /// component is linked as the symbolic-link inode itself.
    NoFollowFinal,
    /// `linkat(..., AT_SYMLINK_FOLLOW)`-like behavior: the complete source pathname is resolved
    /// before the new namespace reference is created.
    FollowFinal,
}

/// Creates one additional durable namespace reference with an explicit final-source follow policy.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. `follow`
/// selects whether the final source symbolic link is preserved as the linked inode or resolved to
/// its target. The persisted selected inode kind then dispatches to the existing regular-file or
/// symbolic-link hard-link transaction. Directories are rejected so namespace parent/cycle
/// invariants remain unchanged. The destination parent follows bounded symbolic-link traversal while
/// the final destination component remains an unfollowed collision-checked namespace key.
///
/// The operation changes only the directory table. Allocator ownership, inode state, regular-file
/// data, and symbolic-link payload data remain unchanged, so filesystem format v5 is unchanged.
///
/// # Errors
/// Returns `InvalidInput` when either pathname is not absolute, when the destination names the root
/// or has an empty final component, when the selected source inode is missing, or when the selected
/// source is a directory. Recovery, checkpoint, bounded pathname resolution, persisted metadata
/// decoding, and durable hard-link I/O errors propagate.
pub fn hard_link_at_path_with_source_follow_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
    follow: HardLinkSourceFollow,
) -> io::Result<RecoveryReport> {
    if !source.starts_with('/') {
        return Err(invalid_input("hard-link source must be an absolute path"));
    }

    recover_journal_and_checkpoint(device, *superblock)?;
    let target = match follow {
        HardLinkSourceFollow::NoFollowFinal => {
            resolve_path_without_following_final_symlink(device, superblock, source)?
        }
        HardLinkSourceFollow::FollowFinal => {
            resolve_path_following_symlinks(device, superblock, source)?
        }
    };
    let target_kind = load_inode_table(device, superblock)?
        .iter()
        .find(|inode| inode.id == target)
        .map(|inode| inode.kind)
        .ok_or_else(|| invalid_input("hard-link source inode is missing"))?;
    let (parent_path, name) = split_destination(destination)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;

    match target_kind {
        InodeKind::File => hard_link_file_journaled(device, superblock, parent, name, target),
        InodeKind::Symlink => hard_link_symlink_journaled(device, superblock, parent, name, target),
        InodeKind::Directory => Err(invalid_input("hard-link source cannot be a directory")),
    }
}

/// Creates one additional durable namespace reference using POSIX-like `link` source semantics.
///
/// This is the no-final-follow convenience surface for
/// [`hard_link_at_path_with_source_follow_journaled`].
///
/// # Errors
/// Propagates all recovery, checkpoint, pathname-resolution, metadata-validation, and durable
/// hard-link I/O errors from [`hard_link_at_path_with_source_follow_journaled`].
pub fn hard_link_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    hard_link_at_path_with_source_follow_journaled(
        device,
        superblock,
        source,
        destination,
        HardLinkSourceFollow::NoFollowFinal,
    )
}

/// Creates one additional durable namespace reference after following the final source symlink.
///
/// This is the bounded format-v5 analogue of `linkat(2)` with `AT_SYMLINK_FOLLOW` and the
/// final-follow convenience surface for [`hard_link_at_path_with_source_follow_journaled`].
///
/// # Errors
/// Propagates all recovery, checkpoint, pathname-resolution, metadata-validation, and durable
/// hard-link I/O errors from [`hard_link_at_path_with_source_follow_journaled`].
pub fn hard_link_following_source_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    hard_link_at_path_with_source_follow_journaled(
        device,
        superblock,
        source,
        destination,
        HardLinkSourceFollow::FollowFinal,
    )
}

/// Creates one additional durable namespace reference to a regular file using pathnames.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution so source and
/// destination-parent lookup never derive inode IDs from a partially replayed namespace. The source
/// pathname is then resolved with the repository-wide bounded symbolic-link expansion rules,
/// including the final component. The destination is split into a parent pathname plus final
/// basename; only the parent is resolved, so an existing final destination remains a collision.
/// Publication is delegated to [`hard_link_file_journaled`], preserving its directory-only WAL
/// transaction and leaving allocator ownership plus the inode image unchanged.
///
/// # Errors
/// Returns `InvalidInput` when either pathname is not absolute, when the destination names the root
/// or has an empty final component, or when the resolved source is not a regular file. Recovery,
/// checkpoint, source or parent path-resolution errors and all [`hard_link_file_journaled`] durable
/// I/O errors propagate.
pub fn hard_link_file_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    if !source.starts_with('/') {
        return Err(invalid_input("hard-link source must be an absolute path"));
    }

    recover_journal_and_checkpoint(device, *superblock)?;
    let target = resolve_path_following_symlinks(device, superblock, source)?;
    let (parent_path, name) = split_destination(destination)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;
    hard_link_file_journaled(device, superblock, parent, name, target)
}

/// Creates one additional durable namespace reference to a symbolic-link inode using pathnames.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution so source and
/// destination-parent lookup observe one recovered namespace. Intermediate source symlinks are then
/// followed, but the final source component is deliberately not followed. The final source inode
/// must itself be a symbolic link. The destination parent is resolved with the normal bounded
/// symlink-following rules while its final basename remains a collision-checked namespace key.
/// Publication is delegated to [`hard_link_symlink_journaled`], which validates the persisted
/// symlink payload before issuing the directory-only WAL update.
///
/// # Errors
/// Returns `InvalidInput` when either pathname is not absolute, when the destination names the root
/// or has an empty final component, or when the final source inode is not a symbolic link. Recovery,
/// checkpoint, source or parent path-resolution errors, symlink corruption, and all
/// [`hard_link_symlink_journaled`] durable I/O errors propagate.
pub fn hard_link_symlink_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    if !source.starts_with('/') {
        return Err(invalid_input("hard-link source must be an absolute path"));
    }

    recover_journal_and_checkpoint(device, *superblock)?;
    let target = resolve_path_without_following_final_symlink(device, superblock, source)?;
    let (parent_path, name) = split_destination(destination)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;
    hard_link_symlink_journaled(device, superblock, parent, name, target)
}

fn split_destination(path: &str) -> io::Result<(&str, &str)> {
    if !path.starts_with('/') {
        return Err(invalid_input(
            "hard-link destination must be an absolute path",
        ));
    }
    if path == "/" {
        return Err(invalid_input(
            "hard-link destination cannot be the root path",
        ));
    }

    let (parent, name) = path
        .rsplit_once('/')
        .ok_or_else(|| invalid_input("hard-link destination must contain a final component"))?;
    if name.is_empty() {
        return Err(invalid_input(
            "hard-link destination final component is empty",
        ));
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
    fn destination_split_preserves_root_and_nested_parent_paths() {
        assert_eq!(split_destination("/link").unwrap(), ("/", "link"));
        assert_eq!(
            split_destination("/dir/sub/link").unwrap(),
            ("/dir/sub", "link")
        );
    }

    #[test]
    fn destination_split_rejects_non_absolute_root_and_trailing_slash() {
        for path in ["link", "/", "/dir/"] {
            assert_eq!(
                split_destination(path).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
