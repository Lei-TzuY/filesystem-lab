use std::collections::HashSet;
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

/// Atomically overwrites multiple existing logical blocks of one regular file.
///
/// Every changed full-block image is published in one WAL transaction. The operation does not
/// modify allocator ownership, inode metadata, namespace state, or the on-disk format. After the
/// committed images are replayed and made durable, the fixed journal reservation is checkpointed
/// before successful return so it can be reused immediately.
///
/// Format v5 does not persist byte lengths, so this API deliberately remains block-granular. It
/// does not extend the file, allocate blocks, create sparse holes, or provide partial-block writes.
///
/// # Errors
///
/// Returns `InvalidInput` when the batch is empty, contains duplicate logical indices, the inode is
/// missing or not a regular file, or any logical index is outside the inode block list. Returns
/// `InvalidData` when an inode-referenced physical block is not allocator-owned. Journal-capacity,
/// I/O, recovery, checkpoint, and flush failures are propagated. A failure may occur after commit
/// becomes durable; callers must recover and checkpoint before interpreting the final data state.
pub fn write_file_blocks_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    writes: &[(usize, [u8; BLOCK_SIZE])],
) -> io::Result<RecoveryReport> {
    if writes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-data overwrite batch must not be empty",
        ));
    }

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

    let allocator = load_allocator(device, superblock)?;
    let mut seen = HashSet::with_capacity(writes.len());
    let mut changed = Vec::with_capacity(writes.len());

    for (logical_index, image) in writes.iter().copied() {
        if !seen.insert(logical_index) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "file-data overwrite batch contains a duplicate logical block index",
            ));
        }
        let block = *inode.blocks.get(logical_index).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "file-data logical block index is out of range",
            )
        })?;
        let owned = allocator
            .is_owned(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if !owned {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "file-data inode references an unowned block",
            ));
        }

        let mut current = [0_u8; BLOCK_SIZE];
        device.read_block(block, &mut current)?;
        if current != image {
            changed.push((block, image));
        }
    }

    if changed.is_empty() {
        return Ok(RecoveryReport::default());
    }

    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for (block, image) in changed.iter().copied() {
        log.write(txid, block, image)?;
    }
    log.commit(txid)?;
    store_journal_image(device, *superblock, log.entries())?;

    let report = recover_journal_and_checkpoint(device, *superblock)?;
    if report.committed_transactions != 1 || report.home_writes != changed.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "multi-block file-data transaction recovery report is inconsistent",
        ));
    }
    Ok(report)
}
