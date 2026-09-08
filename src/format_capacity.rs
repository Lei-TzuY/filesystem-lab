use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::directory_codec::{DIRECTORY_ENTRY_HEADER_LEN, DIRECTORY_NAME_MAX_BYTES};
use crate::directory_table::DIRECTORY_TABLE_HEADER_LEN;
use crate::format::{Superblock, DEFAULT_DIRECTORY_BLOCKS, DEFAULT_INODE_BLOCKS};
use crate::format_geometry::format_device_with_metadata_blocks;
use crate::inode_codec::INODE_RECORD_HEADER_LEN;
use crate::inode_table::INODE_TABLE_HEADER_LEN;

/// Formats a fresh v5 filesystem whose inode-table reservation can hold at least
/// `inode_capacity` blockless inode records.
///
/// The guarantee is deliberately narrow: each additional physical block reference
/// consumes eight more bytes in the inode table. Callers planning block-bearing
/// inodes must account for that separately.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] when `inode_capacity` is zero or when
/// the required inode-table geometry overflows. Propagates formatting errors from
/// the underlying metadata-geometry formatter.
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
    let inode_blocks = blocks_for_bytes(records_bytes, "inode capacity overflow")?;

    format_device_with_metadata_blocks(
        device,
        journal_blocks,
        inode_blocks,
        DEFAULT_DIRECTORY_BLOCKS,
    )
}

/// Formats a fresh v5 filesystem whose directory-table reservation can hold at least
/// `entry_capacity` entries whose names are no longer than `max_name_bytes` bytes.
///
/// The planner uses the current `DNT1` record and directory-table headers, so the
/// guarantee is a worst-case byte reservation for the requested entry count. The
/// default inode-table reservation is retained; callers requiring more inode-table
/// capacity should use [`crate::format_geometry::format_device_with_metadata_blocks`]
/// with explicit geometry.
///
/// # Errors
///
/// Returns [`io::ErrorKind::InvalidInput`] when either capacity is zero,
/// `max_name_bytes` exceeds the directory-entry codec limit, or checked size
/// arithmetic overflows. Propagates formatting errors from the underlying
/// metadata-geometry formatter.
pub fn format_device_for_directory_capacity(
    device: &mut impl BlockDevice,
    journal_blocks: u64,
    entry_capacity: usize,
    max_name_bytes: usize,
) -> io::Result<Superblock> {
    if entry_capacity == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory entry capacity must be non-zero",
        ));
    }
    if max_name_bytes == 0 || max_name_bytes > DIRECTORY_NAME_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory maximum name length is invalid",
        ));
    }

    let record_bytes = DIRECTORY_ENTRY_HEADER_LEN
        .checked_add(max_name_bytes)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "directory capacity overflow")
        })?;
    let table_bytes = entry_capacity
        .checked_mul(record_bytes)
        .and_then(|bytes| bytes.checked_add(DIRECTORY_TABLE_HEADER_LEN))
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "directory capacity overflow")
        })?;
    let directory_blocks = blocks_for_bytes(table_bytes, "directory capacity overflow")?;

    format_device_with_metadata_blocks(
        device,
        journal_blocks,
        DEFAULT_INODE_BLOCKS,
        directory_blocks,
    )
}

fn blocks_for_bytes(bytes: usize, overflow_message: &'static str) -> io::Result<u64> {
    let blocks = bytes
        .checked_add(BLOCK_SIZE - 1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, overflow_message))?
        / BLOCK_SIZE;
    u64::try_from(blocks).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, overflow_message))
}
