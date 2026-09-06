use std::io;

use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::directory_tx::store_directory_table_journaled;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::recovery::RecoveryReport;
use crate::symlink::read_symlink;

/// Removes exactly one non-final namespace reference to a regular-file inode.
///
/// Format v5 derives a file's link count from directory entries. This operation changes only the
/// directory table: the inode and allocator/data ownership remain unchanged. It deliberately
/// rejects removing the final link, which remains the allocation/inode/directory unlink lifecycle.
///
/// # Errors
///
/// Returns `InvalidInput` when the selected entry is missing, targets a non-file inode, or is the
/// target's final namespace reference. Metadata decoding, WAL, checkpoint, and device errors are
/// propagated.
pub fn unlink_nonfinal_file_link_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
) -> io::Result<RecoveryReport> {
    unlink_nonfinal_link_journaled(device, superblock, parent, name, InodeKind::File, false)
}

/// Removes exactly one non-final namespace reference to a symbolic-link inode.
///
/// The persisted symlink payload is validated before WAL publication. The operation changes only
/// the directory table, preserving the symlink inode, its target block, and allocator ownership.
/// Final-link deletion remains the separate symlink final-unlink lifecycle.
///
/// # Errors
///
/// Returns `InvalidInput` when the selected entry is missing, targets a non-symlink inode, or is the
/// target's final namespace reference. Corrupt symlink payloads return `InvalidData`. Metadata
/// decoding, WAL, checkpoint, and device errors are propagated.
pub fn unlink_nonfinal_symlink_link_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
) -> io::Result<RecoveryReport> {
    unlink_nonfinal_link_journaled(device, superblock, parent, name, InodeKind::Symlink, true)
}

fn unlink_nonfinal_link_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
    expected_kind: InodeKind,
    validate_symlink_payload: bool,
) -> io::Result<RecoveryReport> {
    let inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;
    let index = entries
        .iter()
        .position(|entry| entry.parent == parent && entry.name == name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hard-link entry is missing"))?;
    let target = entries[index].target;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == target)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "hard-link target inode is missing",
            )
        })?;
    if inode.kind != expected_kind {
        return invalid("non-final hard-link unlink target has the wrong inode kind");
    }
    if entries
        .iter()
        .filter(|entry| entry.target == target)
        .count()
        < 2
    {
        return invalid("non-final hard-link unlink cannot remove the final reference");
    }
    if validate_symlink_payload {
        read_symlink(device, superblock, target)?;
    }

    entries.remove(index);
    let report = store_directory_table_journaled(device, superblock, &entries)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    Ok(report)
}

fn invalid<T>(message: &'static str) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidInput, message))
}
