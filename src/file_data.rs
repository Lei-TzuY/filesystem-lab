use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;

/// Reads one existing logical data block from a durable regular file.
///
/// Format v5 does not persist a byte length, so this API deliberately exposes only block-granular
/// I/O over block references already present in the inode. It rejects metadata/data ownership
/// disagreement instead of reading through an inconsistent inode reference.
///
/// # Errors
///
/// Returns `InvalidInput` when the inode is missing, is not a regular file, or the logical block
/// index is outside the inode's existing block list. Returns `InvalidData` when the referenced
/// physical block is not currently allocator-owned. Underlying decode and block-device errors are
/// propagated.
pub fn read_file_block(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    file_block_index: usize,
) -> io::Result<[u8; BLOCK_SIZE]> {
    let block = resolve_owned_file_block(device, superblock, inode_id, file_block_index)?;
    let mut data = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut data)?;
    Ok(data)
}

/// Journals one full-block overwrite of an existing regular-file block.
///
/// The target physical block must already be referenced by the inode and owned by the allocator.
/// The new 4 KiB image is committed through the existing WAL before recovery installs it at the
/// data-block home location. After the home write is durable, the same operation checkpoints the
/// fixed journal reservation before returning success so the reservation can be reused immediately.
/// A crash before durable commit leaves the old block durable; a crash after durable commit remains
/// recoverable to the complete new block image even if it happens during home replay or checkpoint.
///
/// This bounded format-v5 slice intentionally does not allocate blocks, change inode metadata, or
/// model byte lengths. Extending a file, partial-block writes, sparse files, and append semantics are
/// separate lifecycle contracts.
///
/// # Errors
///
/// Returns `InvalidInput` when the inode is missing, is not a regular file, or the logical block
/// index is outside the inode's existing block list. Returns `InvalidData` when allocator ownership
/// disagrees with the inode reference. Journal-capacity, journal I/O, recovery, home-write,
/// checkpoint, and flush failures are propagated. A failure may occur after commit is durable;
/// callers must recover and checkpoint the journal before interpreting the operation as complete.
pub fn write_file_block_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    file_block_index: usize,
    data: [u8; BLOCK_SIZE],
) -> io::Result<RecoveryReport> {
    let block = resolve_owned_file_block(device, superblock, inode_id, file_block_index)?;

    let mut current = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut current)?;
    if current == data {
        return Ok(RecoveryReport::default());
    }

    let mut log = JournalLog::new();
    let txid = log.begin()?;
    log.write(txid, block, data)?;
    log.commit(txid)?;
    store_journal_image(device, *superblock, log.entries())?;

    let report = recover_journal_and_checkpoint(device, *superblock)?;
    if report.committed_transactions != 1 || report.home_writes != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file-data transaction recovery report is inconsistent",
        ));
    }
    Ok(report)
}

fn resolve_owned_file_block(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    file_block_index: usize,
) -> io::Result<u64> {
    let inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "file-data target inode is missing",
            )
        })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-data target must be a regular file",
        ));
    }
    let block = *inode.blocks.get(file_block_index).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-data logical block index is out of range",
        )
    })?;

    let allocator = load_allocator(device, superblock)?;
    let owned = allocator
        .is_owned(block)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !owned {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file-data inode references an unowned block",
        ));
    }
    Ok(block)
}
