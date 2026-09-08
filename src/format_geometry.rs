use std::io;

use crate::allocation_disk::initialize_allocation_region;
use crate::block::BlockDevice;
use crate::directory_table::initialize_directory_table_region;
use crate::format::{Superblock, SUPERBLOCK_BLOCK};
use crate::inode_table::initialize_inode_table_region;

/// Writes a fresh format-v5 filesystem using an explicit journal reservation.
///
/// This preserves the version-5 on-disk layout while allowing callers to reserve enough WAL space
/// for bounded transactions whose complete redo image exceeds the default journal geometry.
/// Allocation, inode, and directory regions are initialized before the superblock is published.
///
/// # Errors
///
/// Returns an error when the requested journal geometry is invalid, the device is too small,
/// metadata initialization fails, or an underlying write/flush operation fails.
pub fn format_device_with_journal_blocks(
    device: &mut impl BlockDevice,
    journal_blocks: u64,
) -> io::Result<Superblock> {
    let defaults = Superblock::with_journal_blocks(device.block_count(), journal_blocks)?;
    format_device_with_metadata_blocks(
        device,
        journal_blocks,
        defaults.inode_blocks,
        defaults.directory_blocks,
    )
}

/// Writes a fresh format-v5 filesystem using explicit journal, inode, and directory reservations.
///
/// This is the format-time entry point for bounded namespace scaling experiments. The persisted v5
/// superblock already carries all three reservation lengths; exposing them here lets callers create
/// larger inode or directory tables without manually publishing partially initialized geometry.
/// Allocation, inode, and directory regions are initialized before block zero is written, preserving
/// the existing metadata-prefix publication rule.
///
/// # Errors
///
/// Returns an error for zero-sized or oversized metadata reservations, metadata initialization
/// failures, or underlying block-device I/O failures. Invalid geometry is rejected before the
/// superblock is published.
pub fn format_device_with_metadata_blocks(
    device: &mut impl BlockDevice,
    journal_blocks: u64,
    inode_blocks: u64,
    directory_blocks: u64,
) -> io::Result<Superblock> {
    let superblock = Superblock::with_all_metadata_blocks(
        device.block_count(),
        journal_blocks,
        inode_blocks,
        directory_blocks,
    )?;
    initialize_allocation_region(device, &superblock)?;
    initialize_inode_table_region(device, &superblock)?;
    initialize_directory_table_region(device, &superblock)?;
    device.write_block(SUPERBLOCK_BLOCK, &superblock.encode())?;
    device.flush()?;
    Ok(superblock)
}
