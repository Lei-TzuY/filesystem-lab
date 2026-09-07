use std::io;

use crate::block::BlockDevice;
use crate::format::Superblock;
use crate::hard_link_tx::hard_link_file_journaled;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Creates one additional durable namespace reference to a regular file using pathnames.
///
/// The source pathname is resolved with the repository-wide bounded symbolic-link expansion rules,
/// including the final component. The destination is split into a parent pathname and final
/// basename; only the parent is resolved, so an existing final destination remains a collision.
/// Publication is delegated to [`hard_link_file_journaled`], preserving its directory-only WAL
/// transaction and leaving allocator ownership plus the inode image unchanged.
///
/// # Errors
/// Returns `InvalidInput` when either pathname is not absolute, when the destination names the root
/// or has an empty final component, or when the resolved source is not a regular file. Source or
/// parent path-resolution errors and all [`hard_link_file_journaled`] durable I/O errors propagate.
pub fn hard_link_file_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    if !source.starts_with('/') {
        return Err(invalid_input("hard-link source must be an absolute path"));
    }

    let target = resolve_path_following_symlinks(device, superblock, source)?;
    let (parent_path, name) = split_destination(destination)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;
    hard_link_file_journaled(device, superblock, parent, name, target)
}

fn split_destination(path: &str) -> io::Result<(&str, &str)> {
    if !path.starts_with('/') {
        return Err(invalid_input(
            "hard-link destination must be an absolute path",
        ));
    }
    if path == "/" {
        return Err(invalid_input("hard-link destination cannot be the root path"));
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
