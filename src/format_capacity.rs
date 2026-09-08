use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::{Superblock, DEFAULT_DIRECTORY_BLOCKS};
use crate::format_geometry::format_device_with_metadata_blocks;
use crate::inode_codec::INODE_RECORD_HEADER_LEN;
use crate::inode_table::INODE_TABLE_HEADER_LEN;

/// Formats a fresh v5 filesystem whose inode-table reservation can hold at least
/// `inode_capacity` blockless inode records.
///
/// The guarantee is deliberately narrow: each additional physical block reference
/// consumes eight more bytes in the inode table. Callers planning block-bearing
/// inodes must account for that separately.
pub fn format_device_for_blockless_inode_capacity(
    device: &mut impl BlockDevice,
    journal_blocks: u64,
    inode_capacity: usize,
) -> io::Result<Superblock> {
    if inode_capacity == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inode capacity must be non-zero",
        ));
    }

    let records_bytes = inode_capacity
        .checked_mul(INODE_RECORD_HEADER_LEN)
        .and_then(|bytes| bytes.checked_add(INODE_TABLE_HEADER_LEN))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "inode capacity overflow"))?;
    let inode_blocks = records_bytes
        .checked_add(BLOCK_SIZE - 1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "inode capacity overflow"))?
        / BLOCK_SIZE;
    let inode_blocks = u64::try_from(inode_blocks)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "inode capacity overflow"))?;

    format_device_with_metadata_blocks(
        device,
        journal_blocks,
        inode_blocks,
        DEFAULT_DIRECTORY_BLOCKS,
    )
}
