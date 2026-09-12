use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::{load_allocator, store_allocator};
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

/// Replaces one non-empty existing logical-block range with a caller-provided sequence whose fresh
/// physical blocks are allocated as one contiguous run.
///
/// Fresh blocks are reserved before displaced blocks are released, so the replacement run never
/// aliases blocks still referenced by the pre-transaction inode image. Allocation ownership, the
/// resized inode block vector, and all replacement data images are published through one WAL
/// transaction. Format v5 continues to persist an explicit block vector; contiguity is an
/// allocation-time guarantee for this operation, not a persistent extent invariant.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty replacement, a zero-length destination range, a missing or
/// non-file inode, a destination range outside existing logical blocks, range overflow, block-count
/// conversion failure, or lack of a sufficiently large contiguous free run. Returns `InvalidData`
/// when allocator ownership disagrees with displaced inode references or when a contiguous block
/// address overflows. Journal-capacity, encoding, recovery, checkpoint, flush, and block-device I/O
/// failures are propagated.
pub fn replace_file_blocks_contiguous_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    start: usize,
    remove_count: usize,
    replacements: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    if remove_count == 0 || replacements.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous block replacement requires non-empty removed and replacement ranges",
        ));
    }
    let end = start.checked_add(remove_count).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous block replacement destination range overflows usize",
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
                "contiguous replacement inode is missing",
            )
        })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous block replacement target must be a regular file",
        ));
    }
    if end > inode.blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous block replacement range exceeds existing logical blocks",
        ));
    }

    let (new_blocks, displaced_blocks) =
        prepare_mapping(&mut allocator, inode, start, end, replacements.len())?;

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &allocator)?;
    store_inode_table(&mut capture, superblock, &inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "contiguous block replacement image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "contiguous block replacement image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty(
        "contiguous block replacement image rendered outside allocation and inode regions",
    )?;
    changed.extend(new_blocks.iter().copied().zip(replacements.iter().copied()));

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
            "contiguous block replacement recovery report is inconsistent",
        ));
    }

    Ok((new_blocks, displaced_blocks, report))
}

fn prepare_mapping(
    allocator: &mut BlockAllocator,
    inode: &mut PersistedInode,
    start: usize,
    end: usize,
    replacement_len: usize,
) -> io::Result<(Vec<u64>, Vec<u64>)> {
    let displaced_blocks = inode.blocks[start..end].to_vec();
    for block in &displaced_blocks {
        if !allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "contiguous replacement displaced block is not allocator-owned",
            ));
        }
    }

    let replacement_count = u64::try_from(replacement_len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous block replacement exceeds the block address space",
        )
    })?;
    let first_block = allocator
        .allocate_contiguous(replacement_count)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut new_blocks = Vec::with_capacity(replacement_len);
    for index in 0..replacement_len {
        let offset = u64::try_from(index).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "contiguous block replacement exceeds the block address space",
            )
        })?;
        let block = first_block.checked_add(offset).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "contiguous replacement run overflowed block address space",
            )
        })?;
        new_blocks.push(block);
    }
    for block in &displaced_blocks {
        allocator
            .free(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }
    inode.blocks.splice(start..end, new_blocks.iter().copied());
    Ok((new_blocks, displaced_blocks))
}
