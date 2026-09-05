use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::recovery::RecoveryReport;

/// Removes one complete logical block from an existing regular file.
///
/// The selected physical block is released from allocator ownership in the same WAL transaction as
/// the inode block-reference update. Logical blocks after `remove_index` shift left by one position;
/// namespace metadata and the on-disk format are unchanged.
///
/// Format v5 has no persisted byte length, so this is deliberately block-granular. It does not
/// claim byte-range collapse, EOF, sparse-hole, or extent semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for a missing/non-file inode or a logical index outside the existing block
/// vector. Returns `InvalidData` when allocator ownership disagrees with the selected inode reference.
/// Durable metadata decoding, journal-capacity, recovery, checkpoint, home-write, flush, and block-
/// device failures are propagated.
pub fn remove_file_block_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    remove_index: usize,
) -> io::Result<(u64, RecoveryReport)> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;

    let inode = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "file removal target inode is missing",
            )
        })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file removal target must be a regular file",
        ));
    }
    if remove_index >= inode.blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file removal logical index is outside the file",
        ));
    }

    let block = inode.blocks.remove(remove_index);
    allocator
        .free(block)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    let report =
        store_create_metadata_journaled(device, superblock, &allocator, &inodes, &entries)?;
    Ok((block, report))
}
