use std::io;

use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::path_rename::rename_at_path_journaled;
use crate::path_rename_overwrite::rename_overwrite_at_path_journaled;
use crate::recovery::RecoveryReport;

/// Performs one POSIX-like durable pathname rename.
///
/// The operation recovers and checkpoints any older durable WAL before resolving source and
/// destination parent paths. Parent components follow bounded symbolic-link traversal while final
/// components remain unfollowed. If the destination does not exist, publication delegates to the
/// ordinary durable pathname rename. If a different same-kind destination exists, publication
/// delegates to the recovered pathname rename-overwrite dispatcher. If both names already refer to
/// the same inode, the operation is a no-op, matching hard-link alias rename semantics.
///
/// A single terminal slash expresses directory intent and repeated trailing separators are rejected.
/// Directory intent is validated even for the same-inode no-op case. This dispatcher changes no
/// persisted format: format v5 continues to derive hard-link counts from namespace references.
///
/// # Errors
/// Returns `NotFound` when the source entry is absent. Returns `InvalidInput` for malformed paths,
/// invalid directory intent, or endpoint combinations rejected by the delegated durable primitive.
/// Recovery, checkpoint, pathname resolution, metadata decoding, and device I/O failures propagate.
pub fn rename_posix_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    let (normalized_source, source_directory_intent) =
        normalize_directory_intent(source, "rename source")?;
    let (normalized_destination, destination_directory_intent) =
        normalize_directory_intent(destination, "rename destination")?;
    let (source_parent_path, source_name) = split_path(normalized_source, "rename source")?;
    let (destination_parent_path, destination_name) =
        split_path(normalized_destination, "rename destination")?;

    let recovery = recover_journal_and_checkpoint(device, *superblock)?;
    let source_parent =
        resolve_path_following_symlinks(device, superblock, source_parent_path)?;
    let destination_parent =
        resolve_path_following_symlinks(device, superblock, destination_parent_path)?;

    let entries = load_directory_table(device, superblock)?;
    let source_target = entries
        .iter()
        .find(|entry| entry.parent == source_parent && entry.name == source_name)
        .map(|entry| entry.target)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "rename source does not exist"))?;
    let destination_target = entries
        .iter()
        .find(|entry| entry.parent == destination_parent && entry.name == destination_name)
        .map(|entry| entry.target);

    if destination_target == Some(source_target) {
        if source_directory_intent || destination_directory_intent {
            require_directory_inode(device, superblock, source_target)?;
        }
        return Ok(recovery);
    }

    if destination_target.is_some() {
        rename_overwrite_at_path_journaled(device, superblock, source, destination)
    } else {
        rename_at_path_journaled(device, superblock, source, destination)
    }
}

fn require_directory_inode(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
) -> io::Result<()> {
    let inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "rename source inode is missing"))?;
    if inode.kind != InodeKind::Directory {
        return Err(invalid_input(
            "trailing-slash rename requires directory endpoints",
        ));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_accepts_one_terminal_separator_only() {
        assert_eq!(
            normalize_directory_intent("/dir/", "source").unwrap(),
            ("/dir", true)
        );
        assert_eq!(
            normalize_directory_intent("/dir", "source").unwrap(),
            ("/dir", false)
        );
        assert_eq!(
            normalize_directory_intent("/dir//", "source")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
