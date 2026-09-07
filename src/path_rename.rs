use std::io;

use crate::block::BlockDevice;
use crate::format::Superblock;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;
use crate::rename_tx::rename_entry_journaled;

/// Atomically renames one durable namespace entry addressed by absolute pathnames.
///
/// Only the source and destination parent pathnames are resolved through the repository-wide
/// bounded symbolic-link traversal rules. Neither final component is followed, so renaming a
/// symbolic link moves the link inode itself and an existing destination remains a collision.
/// Publication is delegated to [`rename_entry_journaled`], preserving its directory-only WAL
/// transaction and its directory-cycle validation.
///
/// # Errors
/// Returns `InvalidInput` when either pathname is not absolute, names the root, or has an empty
/// final component. Parent-resolution errors and all [`rename_entry_journaled`] validation/durable
/// I/O errors are propagated.
pub fn rename_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    let (old_parent_path, old_name) = split_path(source, "rename source")?;
    let (new_parent_path, new_name) = split_path(destination, "rename destination")?;

    let old_parent = resolve_path_following_symlinks(device, superblock, old_parent_path)?;
    let new_parent = resolve_path_following_symlinks(device, superblock, new_parent_path)?;

    rename_entry_journaled(
        device,
        superblock,
        old_parent,
        old_name,
        new_parent,
        new_name,
    )
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
        return Err(invalid_input(format!(
            "{label} final component is empty"
        )));
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
