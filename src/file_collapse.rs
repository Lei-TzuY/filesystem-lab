use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::recovery::RecoveryReport;

/// Atomically removes a non-empty contiguous logical-block range from a regular file.
///
/// Every selected physical block is released from allocator ownership in the same WAL transaction
/// as the inode block-reference update. Logical blocks after the removed range shift left by
/// `block_count`; inode identity and namespace metadata are preserved.
///
/// Format v5 has no persisted byte length, so this primitive is deliberately block-granular. It does
/// not claim byte-range collapse, EOF, sparse-hole, `fallocate`, or extent semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for a missing/non-file inode, an empty range, arithmetic overflow, or a
/// range outside the existing block vector. Returns `InvalidData` when allocator ownership disagrees
/// with any selected inode reference. Durable metadata decoding, journal-capacity, recovery,
/// checkpoint, home-write, flush, and block-device failures are propagated.
pub fn collapse_file_block_range_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    start_index: usize,
    block_count: usize,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    if block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file collapse range must remove at least one block",
        ));
    }
    let end_index = start_index
        .checked_add(block_count)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "file collapse range index overflow",
            )
        })?;

    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;

    let inode = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "file collapse target inode is missing",
            )
        })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file collapse target must be a regular file",
        ));
    }
    if start_index >= inode.blocks.len() || end_index > inode.blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file collapse range is outside the file",
        ));
    }

    for block in &inode.blocks[start_index..end_index] {
        if !allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file collapse block is not allocator-owned",
            ));
        }
    }

    let released: Vec<u64> = inode.blocks.drain(start_index..end_index).collect();
    for block in &released {
        allocator
            .free(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }

    let report =
        store_create_metadata_journaled(device, superblock, &allocator, &inodes, &entries)?;
    Ok((released, report))
}
