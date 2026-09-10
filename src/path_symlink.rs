use std::io;

use crate::block::BlockDevice;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;
use crate::symlink::create_symlink_journaled;
use crate::symlink_unlink::unlink_symlink_journaled;

/// Creates one durable symbolic link at an absolute destination pathname.
///
/// The destination is split into a parent pathname and final basename. Older committed WAL is
/// recovered and checkpointed before the parent is resolved with the repository-wide bounded
/// symbolic-link expansion rules, so endpoint selection cannot observe stale home namespace state.
/// Creation is then delegated directly to [`create_symlink_journaled`]. The final component is never
/// resolved: an existing entry is a collision, exactly as for the inode-ID-based primitive.
///
/// # Errors
/// Returns `InvalidInput` when the destination is not absolute, names the root, or has an empty
/// final component. Recovery/checkpoint, path-resolution, and all [`create_symlink_journaled`]
/// validation/durable I/O errors are propagated.
pub fn create_symlink_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    destination: &str,
    target: &str,
) -> io::Result<(u64, RecoveryReport)> {
    let (parent_path, name) = split_destination(destination)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;
    create_symlink_journaled(device, superblock, parent, name, target)
}

/// Removes one final symbolic-link namespace entry addressed by an absolute pathname.
///
/// Older committed WAL is recovered and checkpointed before parent resolution. Intermediate
/// components, including the parent itself, then use the repository-wide bounded symlink expansion
/// rules. The final component is intentionally not resolved so unlink removes the link inode named
/// by the pathname rather than its target. Publication is delegated to
/// [`unlink_symlink_journaled`], preserving its allocator/inode/directory WAL transaction.
///
/// # Errors
/// Returns `InvalidInput` when the pathname is not absolute, names the root, or has an empty final
/// component. Recovery/checkpoint, parent-resolution, and all [`unlink_symlink_journaled`]
/// validation/durable I/O errors are propagated.
pub fn unlink_symlink_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<RecoveryReport> {
    let (parent_path, name) = split_destination(path)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;
    unlink_symlink_journaled(device, superblock, parent, name)
}

fn split_destination(path: &str) -> io::Result<(&str, &str)> {
    if !path.starts_with('/') {
        return Err(invalid_input(
            "symlink destination must be an absolute path",
        ));
    }
    if path == "/" {
        return Err(invalid_input("symlink destination cannot be the root path"));
    }

    let (parent, name) = path
        .rsplit_once('/')
        .ok_or_else(|| invalid_input("symlink destination must contain a final component"))?;
    if name.is_empty() {
        return Err(invalid_input(
            "symlink destination final component is empty",
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
