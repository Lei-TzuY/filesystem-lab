use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::recovery::RecoveryReport;
use crate::symlink::validate_symlink_inode;
use crate::unlink_tx::store_unlink_metadata_journaled;

/// Removes one final namespace reference to a persisted one-block symbolic link.
///
/// The operation validates the target payload before mutation, releases exactly the target block,
/// removes the symlink inode and selected namespace entry, and publishes allocation, inode, and
/// directory metadata through one bounded WAL transaction.
///
/// # Errors
/// Returns `InvalidInput` when the selected entry is missing, targets a non-symlink inode, has more
/// than one namespace reference, or the symlink block is not allocator-owned. Corrupt symlink
/// payloads return `InvalidData`. Durable metadata, WAL, recovery, checkpoint, and device errors are
/// propagated.
pub fn unlink_symlink_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
) -> io::Result<RecoveryReport> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;

    let entry_index = entries
        .iter()
        .position(|entry| entry.parent == parent && entry.name == name)
        .ok_or_else(|| invalid_input("symlink entry is missing"))?;
    let target = entries[entry_index].target;
    let inode_index = inodes
        .iter()
        .position(|inode| inode.id == target)
        .ok_or_else(|| invalid_input("symlink target inode is missing"))?;
    let inode = &inodes[inode_index];

    if inode.kind != InodeKind::Symlink {
        return Err(invalid_input("unlink requires a symbolic-link inode"));
    }
    if entries
        .iter()
        .filter(|entry| entry.target == target)
        .count()
        != 1
    {
        return Err(invalid_input(
            "symlink unlink requires exactly one namespace reference",
        ));
    }
    validate_symlink_inode(device, inode)?;
    let block = inode.blocks[0];
    if !allocator
        .is_owned(block)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
    {
        return Err(invalid_input("symlink target block is not allocator-owned"));
    }

    allocator
        .free(block)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    inodes.remove(inode_index);
    entries.remove(entry_index);

    store_unlink_metadata_journaled(device, superblock, &allocator, &inodes, &entries)
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
