use std::io;

use crate::allocation_disk::{load_allocator, store_allocator};
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_overwrite_batch::write_file_blocks_journaled;
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
/// # Errors
/// Returns `InvalidInput` for an empty or cross-block range or invalid file target, and
/// `InvalidData` for ownership disagreement. Durable I/O errors are propagated.
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

/// Atomically writes a byte range across one or more already-existing logical file blocks.
///
/// `start_offset` is relative to `first_block_index`. The range may cross block boundaries, but it
/// may not extend beyond blocks already referenced by the regular-file inode. Complete resulting
/// block images are committed together through the existing multi-block WAL path, so a durable
/// commit recovers the whole byte range rather than a prefix. Format v5 still has no persisted byte
/// length; this operation does not extend files, allocate blocks, or create sparse holes.
///
/// # Errors
/// Returns `InvalidInput` for an empty range, an offset outside the first block, or a range that
/// extends beyond the inode's existing block list. Invalid file targets, ownership disagreement,
/// journal-capacity failures, and durable I/O errors are propagated.
pub fn write_file_range_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    first_block_index: usize,
    start_offset: usize,
    data: &[u8],
) -> io::Result<RecoveryReport> {
    if data.is_empty() || start_offset >= BLOCK_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-data range must be non-empty and start within a block",
        ));
    }
    let last_byte = start_offset.checked_add(data.len()).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "file-data range length overflow")
    })?;
    let block_count = last_byte.div_ceil(BLOCK_SIZE);
    let mut writes = Vec::with_capacity(block_count);
    let mut consumed = 0;
    for relative_index in 0..block_count {
        let logical_index = first_block_index.checked_add(relative_index).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "file-data logical index overflow")
        })?;
        let mut image = read_file_block(device, superblock, inode_id, logical_index)?;
        let begin = if relative_index == 0 { start_offset } else { 0 };
        let available = BLOCK_SIZE - begin;
        let take = available.min(data.len() - consumed);
        image[begin..begin + take].copy_from_slice(&data[consumed..consumed + take]);
        consumed += take;
        writes.push((logical_index, image));
    }
    write_file_blocks_journaled(device, superblock, inode_id, &writes)
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

pub fn append_file_block_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    data: [u8; BLOCK_SIZE],
) -> io::Result<(u64, RecoveryReport)> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let inode = inodes.iter_mut().find(|inode| inode.id == inode_id).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "file-data target inode is missing")
    })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "file-data target must be a regular file"));
    }
    let block = allocator.allocate().map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    inode.blocks.push(block);
    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &allocator)?;
    store_inode_table(&mut capture, superblock, &inodes)?;
    let mut changed = Vec::new();
    capture.collect_changed_range(device, superblock.allocation_range(), "file append image did not render every allocation metadata block", &mut changed)?;
    capture.collect_changed_range(device, superblock.inode_range(), "file append image did not render every inode metadata block", &mut changed)?;
    capture.ensure_empty("file append image rendered outside allocation and inode regions")?;
    changed.push((block, data));
    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for (home_block, image) in changed.iter().copied() { log.write(txid, home_block, image)?; }
    log.commit(txid)?;
    store_journal_image(device, *superblock, log.entries())?;
    let report = recover_journal_and_checkpoint(device, *superblock)?;
    if report.committed_transactions != 1 || report.home_writes != changed.len() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file append recovery report is inconsistent"));
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
    let inode = inodes.iter().find(|inode| inode.id == inode_id).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "file-data target inode is missing")
    })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "file-data target must be a regular file"));
    }
    let block = *inode.blocks.get(file_block_index).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "file-data logical block index is out of range")
    })?;
    let allocator = load_allocator(device, superblock)?;
    let owned = allocator.is_owned(block).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !owned {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "file-data inode references an unowned block"));
    }
    Ok(block)
}
