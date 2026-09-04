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
    let superblock = Superblock::with_journal_blocks(device.block_count(), journal_blocks)?;
    initialize_allocation_region(device, &superblock)?;
    initialize_inode_table_region(device, &superblock)?;
    initialize_directory_table_region(device, &superblock)?;
    device.write_block(SUPERBLOCK_BLOCK, &superblock.encode())?;
    device.flush()?;
    Ok(superblock)
}
