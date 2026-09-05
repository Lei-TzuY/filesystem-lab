use std::{collections::HashSet, io};

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

/// Atomically exchanges equal-length logical-block ranges between two regular files.
///
/// Physical blocks are neither copied nor reallocated. Only the two inode block-reference
/// sequences change, so allocator accounting, namespace state, inode identities, and file block
/// counts remain unchanged. Format v5 has no persisted byte length; this is block-granular only.
///
/// # Errors
/// Returns `InvalidInput` for identical/missing/non-file endpoints, a zero block count, or an
/// out-of-range logical interval. Returns `InvalidData` for duplicate references or allocator
/// ownership disagreement. WAL, checkpoint, codec, and device errors are propagated.
pub fn exchange_file_block_ranges_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    left_inode_id: u64,
    left_index: usize,
    right_inode_id: u64,
    right_index: usize,
    block_count: usize,
) -> io::Result<RecoveryReport> {
    if left_inode_id == right_inode_id || block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "block-range exchange requires distinct files and a non-empty range",
        ));
    }
    let allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let left_pos = inodes
        .iter()
        .position(|inode| inode.id == left_inode_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "left inode is missing"))?;
    let right_pos = inodes
        .iter()
        .position(|inode| inode.id == right_inode_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "right inode is missing"))?;
    if inodes[left_pos].kind != InodeKind::File || inodes[right_pos].kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "block-range exchange endpoints must be regular files",
        ));
    }
    let left_end = left_index.checked_add(block_count).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "left range overflows")
    })?;
    let right_end = right_index.checked_add(block_count).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "right range overflows")
    })?;
    if left_end > inodes[left_pos].blocks.len() || right_end > inodes[right_pos].blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "block-range exchange is beyond file end",
        ));
    }
    let mut seen = HashSet::new();
    for block in inodes[left_pos]
        .blocks
        .iter()
        .chain(&inodes[right_pos].blocks)
        .copied()
    {
        if !seen.insert(block) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exchange endpoints contain duplicate physical-block references",
            ));
        }
        if !allocator
            .is_owned(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "exchange endpoint references a block that is not allocator-owned",
            ));
        }
    }
    let left = inodes[left_pos].blocks[left_index..left_end].to_vec();
    let right = inodes[right_pos].blocks[right_index..right_end].to_vec();
    inodes[left_pos].blocks[left_index..left_end].copy_from_slice(&right);
    inodes[right_pos].blocks[right_index..right_end].copy_from_slice(&left);

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_inode_table(&mut capture, superblock, &inodes)?;
    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "block-range exchange did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("block-range exchange rendered outside inode region")?;
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
            "block-range exchange recovery report is inconsistent",
        ));
    }
    Ok(report)
}
