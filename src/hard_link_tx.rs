use std::io;

use crate::block::BlockDevice;
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::load_directory_table;
use crate::directory_tx::store_directory_table_journaled;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::recovery::RecoveryReport;
use crate::symlink::read_symlink;

/// Adds one additional namespace reference to an existing regular-file inode.
///
/// Format v5 does not persist link counts, so the authoritative link count is the number of
/// directory entries targeting the inode. This bounded operation changes only the directory table:
/// allocator ownership and the inode image remain unchanged. Directory hard links are rejected so
/// the existing parent/cycle invariants remain intact.
///
/// # Errors
///
/// Returns `InvalidInput` when the parent is missing or not a directory, the target is missing or
/// not a regular file, or the destination name already exists in the parent. Directory encoding,
/// journal-capacity, recovery, checkpoint, and block-device errors are propagated.
pub fn hard_link_file_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
    target: u64,
) -> io::Result<RecoveryReport> {
    validate_link_endpoints(device, superblock, parent, name, target, InodeKind::File)?;
    publish_link(device, superblock, parent, name, target)
}

/// Adds one additional namespace reference to an existing symbolic-link inode.
///
/// The symbolic-link target remains opaque and unchanged. Before publishing the directory-only WAL
/// transaction, this operation validates the persisted one-block symlink payload through
/// `read_symlink`. Allocation ownership, inode state, and target data are therefore preserved.
///
/// # Errors
///
/// Returns `InvalidInput` when the parent is missing or not a directory, the target is missing or
/// not a symbolic link, or the destination name already exists. Corrupt symlink payloads return
/// `InvalidData`. Directory encoding, WAL, checkpoint, recovery, and device errors are propagated.
pub fn hard_link_symlink_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
    target: u64,
) -> io::Result<RecoveryReport> {
    validate_link_endpoints(device, superblock, parent, name, target, InodeKind::Symlink)?;
    read_symlink(device, superblock, target)?;
    publish_link(device, superblock, parent, name, target)
}

fn validate_link_endpoints(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
    target: u64,
    required_kind: InodeKind,
) -> io::Result<()> {
    let inodes = load_inode_table(device, superblock)?;
    let parent_inode = inodes
        .iter()
        .find(|inode| inode.id == parent)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "hard-link parent inode is missing",
            )
        })?;
    if parent_inode.kind != InodeKind::Directory {
        return invalid("hard-link parent must be a directory");
    }
    let target_inode = inodes
        .iter()
        .find(|inode| inode.id == target)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "hard-link target inode is missing",
            )
        })?;
    if target_inode.kind != required_kind {
        return invalid("hard-link target has the wrong inode kind");
    }

    let entries = load_directory_table(device, superblock)?;
    if entries
        .iter()
        .any(|entry| entry.parent == parent && entry.name == name)
    {
        return invalid("hard-link destination already exists");
    }
    Ok(())
}

fn publish_link(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
    target: u64,
) -> io::Result<RecoveryReport> {
    let mut entries = load_directory_table(device, superblock)?;
    entries.push(PersistedDirectoryEntry {
        parent,
        target,
        name: name.to_owned(),
    });

    let report = store_directory_table_journaled(device, superblock, &entries)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    Ok(report)
}

fn invalid<T>(message: &'static str) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidInput, message))
}
