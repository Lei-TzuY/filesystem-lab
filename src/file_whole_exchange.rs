use std::{collections::HashSet, io};

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

fn publish_whole_file_exchange(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inodes: &[PersistedInode],
) -> io::Result<RecoveryReport> {
    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_inode_table(&mut capture, superblock, inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "whole-file exchange did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("whole-file exchange rendered outside inode region")?;

    if changed.is_empty() {
        return Ok(RecoveryReport::default());
    }

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
            "whole-file exchange recovery report is inconsistent",
        ));
    }
    Ok(report)
}

/// Atomically exchanges the complete persisted logical-block vectors of two regular files.
///
/// The operation moves physical block references between inode records without copying data,
/// allocating blocks, or changing allocator ownership. Unlike range exchange, either endpoint may
/// be empty. Exchanging two empty files is a validated no-op that publishes no WAL transaction.
/// Namespace state and inode identities are unchanged.
///
/// Format v5 has no persisted byte length, so "whole file" means the complete sequence of persisted
/// 4 KiB logical blocks. This primitive does not define partial-final-block, sparse-hole, extent,
/// reflink, or byte-EOF semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for identical, missing, or non-file inode endpoints. Returns
/// `InvalidData` if either inode contains duplicate physical references, the endpoints share a
/// physical block, or any referenced block is not allocator-owned. Inode encoding/capacity, WAL,
/// checkpoint, and device errors are propagated.
pub fn exchange_complete_file_blocks_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    left_inode_id: u64,
    right_inode_id: u64,
) -> io::Result<RecoveryReport> {
    if left_inode_id == right_inode_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "whole-file exchange requires distinct file inodes",
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
            "whole-file exchange endpoints must both be regular files",
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
                "whole-file exchange endpoints contain duplicate physical-block references",
            ));
        }
        if !allocator
            .is_owned(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "whole-file exchange endpoint references a block that is not allocator-owned",
            ));
        }
    }

    if inodes[left_pos].blocks.is_empty() && inodes[right_pos].blocks.is_empty() {
        return Ok(RecoveryReport::default());
    }

    let left_blocks = std::mem::take(&mut inodes[left_pos].blocks);
    let right_blocks = std::mem::replace(&mut inodes[right_pos].blocks, left_blocks);
    inodes[left_pos].blocks = right_blocks;

    publish_whole_file_exchange(device, superblock, &inodes)
}
