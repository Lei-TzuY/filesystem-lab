use std::io;

use crate::allocation_disk::{load_allocator, store_allocator};
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

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
/// # Errors
///
/// Returns `InvalidInput` when the inode is missing, is not a regular file, or the logical block
/// index is outside the inode's existing block list. Returns `InvalidData` when allocator ownership
/// disagrees with the inode reference. Journal-capacity, journal I/O, recovery, home-write,
/// checkpoint, and flush failures are propagated.
pub fn write_file_block_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    file_block_index: usize,
    data: [u8; BLOCK_SIZE],
) -> io::Result<RecoveryReport> {
    let block = resolve_owned_file_block(device, superblock, inode_id, file_block_index)?;
    journal_block_image(device, superblock, block, &data)
}

/// Journals a byte-range read-modify-write within one existing regular-file block.
///
/// Format v5 has no persisted byte length, so the write must stay inside one already referenced
/// logical block. The complete resulting 4 KiB image is committed to the WAL, preserving atomic
/// old-or-new block visibility across crashes. Empty and cross-block ranges are rejected before WAL
/// publication; this operation does not extend files, allocate blocks, or create sparse holes.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty or cross-block range, a missing or non-file inode, or an
/// out-of-range logical block index. Returns `InvalidData` when allocator ownership disagrees with
/// the inode reference or recovery reports an inconsistent transaction. Journal, recovery,
/// checkpoint, and block-device failures are propagated.
pub fn write_file_block_range_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    file_block_index: usize,
    offset: usize,
    data: &[u8],
) -> io::Result<RecoveryReport> {
    if data.is_empty() || offset >= BLOCK_SIZE || data.len() > BLOCK_SIZE - offset {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "partial file-data write must be non-empty and stay within one block",
        ));
    }
    let block = resolve_owned_file_block(device, superblock, inode_id, file_block_index)?;
    let mut image = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut image)?;
    image[offset..offset + data.len()].copy_from_slice(data);
    journal_block_image(device, superblock, block, &image)
}

fn journal_block_image(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    block: u64,
    data: &[u8; BLOCK_SIZE],
) -> io::Result<RecoveryReport> {
    let mut current = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut current)?;
    if current == *data {
        return Ok(RecoveryReport::default());
    }
    let mut log = JournalLog::new();
    let txid = log.begin()?;
    log.write(txid, block, *data)?;
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

/// Appends one complete logical block to an existing regular file in one WAL transaction.
///
/// The operation allocates exactly one previously free data block, appends that physical block to
/// the inode's block list, and installs the caller-provided 4 KiB data image. Allocation metadata,
/// inode metadata, and the new data block are committed together before any home location changes.
/// After replay makes all three home images durable, the fixed journal reservation is checkpointed
/// before successful return.
///
/// Format v5 still has no byte-length field, so this is deliberately block-granular append rather
/// than POSIX `write(2)` append. It does not provide partial-block writes, sparse files, or byte-size
/// truncation semantics.
///
/// # Errors
///
/// Returns `InvalidInput` when the inode is missing or is not a regular file, or when no free data
/// block remains. Encoding, journal-capacity, journal I/O, recovery, home-write, checkpoint, and
/// flush failures are propagated.
pub fn append_file_block_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    data: [u8; BLOCK_SIZE],
) -> io::Result<(u64, RecoveryReport)> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter_mut()
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
    let block = allocator
        .allocate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    inode.blocks.push(block);
    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &allocator)?;
    store_inode_table(&mut capture, superblock, &inodes)?;
    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "file append image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "file append image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("file append image rendered outside allocation and inode regions")?;
    changed.push((block, data));
    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for (home_block, image) in changed.iter().copied() {
        log.write(txid, home_block, image)?;
    }
    log.commit(txid)?;
    store_journal_image(device, *superblock, log.entries())?;
    let report = recover_journal_and_checkpoint(device, *superblock)?;
    if report.committed_transactions != 1 || report.home_writes != changed.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file append recovery report is inconsistent",
        ));
    }
    Ok((block, report))
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
