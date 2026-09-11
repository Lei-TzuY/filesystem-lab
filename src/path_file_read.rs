use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_range_read::read_file_range;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;

/// Reads every complete logical block persisted by the regular file named by an absolute pathname.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. Resolution
/// follows intermediate and final symbolic links. The resolved inode must be a regular file. A
/// zero-block file returns an empty vector; otherwise the operation delegates the complete block
/// range to the existing range-read primitive so allocator ownership validation remains centralized.
///
/// Format v5 does not persist a byte EOF. Consequently this API returns exactly
/// `logical_blocks * BLOCK_SIZE` bytes and does not trim padding, infer a partial final block, or
/// synthesize sparse holes. It changes no durable state or on-disk format.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, bounded pathname lookup, persisted inode-table decode
/// errors, and existing regular-file range-read validation. Returns `InvalidInput` when the resolved
/// inode is not a regular file and `InvalidData` if lookup resolves an inode absent from the table.
pub fn read_file_blocks_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<Vec<u8>> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    let inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "resolved inode is missing"))?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resolved pathname is not a regular file",
        ));
    }
    if inode.blocks.is_empty() {
        return Ok(Vec::new());
    }
    let len = inode.blocks.len().checked_mul(BLOCK_SIZE).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "whole-file logical-block byte length overflow",
        )
    })?;
    read_file_range(device, superblock, inode_id, 0, 0, len)
}
