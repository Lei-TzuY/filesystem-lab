use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::store_allocator;
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::store_directory_table;
use crate::format::Superblock;
use crate::inode_codec::PersistedInode;
use crate::inode_table::store_inode_table;
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

/// Persists create metadata plus one initial file-data block in one bounded WAL transaction.
///
/// The supplied allocator must already own `data_block`, and the desired inode table must reference
/// that physical block exactly once. Allocation, inode, directory, and data-block home images are
/// all logged before commit publication, so recovery cannot expose a reachable inode whose first
/// data block contains an uncommitted image.
///
/// # Errors
/// Returns `InvalidInput` for geometry disagreement, invalid allocator ownership, an invalid desired
/// inode reference count, or insufficient journal capacity. Metadata encoding, journal, recovery,
/// checkpoint, and device I/O failures are propagated.
pub fn store_create_with_data_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    allocator: &BlockAllocator,
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
    data_block: u64,
    data: &[u8; BLOCK_SIZE],
) -> io::Result<RecoveryReport> {
    if device.block_count() != superblock.total_blocks {
        return Err(invalid_input(
            "atomic create-with-data device geometry does not match superblock",
        ));
    }
    allocator.validate()?;
    if !allocator.is_owned(data_block)? {
        return Err(invalid_input(
            "atomic create-with-data block is not allocator-owned",
        ));
    }

    let references = inodes
        .iter()
        .flat_map(|inode| inode.blocks.iter())
        .filter(|block| **block == data_block)
        .count();
    if references != 1 {
        return Err(invalid_input(
            "atomic create-with-data block must have exactly one inode owner",
        ));
    }

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, allocator)?;
    store_inode_table(&mut capture, superblock, inodes)?;
    store_directory_table(&mut capture, superblock, entries)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "atomic create-with-data image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "atomic create-with-data image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.directory_range(),
        "atomic create-with-data image did not render every directory metadata block",
        &mut changed,
    )?;
    capture.ensure_empty(
        "atomic create-with-data image rendered outside allocation, inode, and directory regions",
    )?;

    let mut current = [0_u8; BLOCK_SIZE];
    device.read_block(data_block, &mut current)?;
    if current != *data {
        changed.push((data_block, *data));
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
            "atomic create-with-data recovery report does not match one complete transaction",
        ));
    }
    Ok(report)
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
