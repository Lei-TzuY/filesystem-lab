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

/// Appends multiple complete logical blocks to an existing regular file in one contiguous run.
///
/// Allocation ownership, inode block-list growth, and every new data image are published through one
/// WAL transaction. The allocator chooses the lowest-numbered free run large enough for the entire
/// append. Format v5 still persists an explicit block vector, so contiguity is an allocation-time
/// guarantee for this operation only and is not a persistent extent invariant.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty append, a missing/non-file inode, a block-count conversion
/// failure, or lack of a sufficiently large contiguous free run. Journal-capacity, encoding, I/O,
/// recovery, checkpoint, and flush failures are propagated. A failure may occur after commit is
/// durable; callers must recover and checkpoint before interpreting home state.
pub fn append_file_blocks_contiguous_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    data_blocks: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    if data_blocks.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous multi-block append requires at least one data block",
        ));
    }
    let block_count = u64::try_from(data_blocks.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous multi-block append exceeds the block address space",
        )
    })?;

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

    let first_block = allocator
        .allocate_contiguous(block_count)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut blocks = Vec::with_capacity(data_blocks.len());
    for index in 0..data_blocks.len() {
        let offset = u64::try_from(index).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "contiguous multi-block append exceeds the block address space",
            )
        })?;
        let block = first_block.checked_add(offset).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "contiguous allocation run overflowed block address space",
            )
        })?;
        inode.blocks.push(block);
        blocks.push(block);
    }

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &allocator)?;
    store_inode_table(&mut capture, superblock, &inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "contiguous multi-block append image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "contiguous multi-block append image did not render every inode metadata block",
        &mut changed,
    )?;
    capture
        .ensure_empty(
            "contiguous multi-block append image rendered outside allocation and inode regions",
        )?;
    changed.extend(blocks.iter().copied().zip(data_blocks.iter().copied()));

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
            "contiguous multi-block append recovery report is inconsistent",
        ));
    }

    Ok((blocks, report))
}
