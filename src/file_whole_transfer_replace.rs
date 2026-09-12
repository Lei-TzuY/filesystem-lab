use std::{collections::HashSet, io};

use crate::allocation_disk::{load_allocator, store_allocator};
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

fn publish_whole_file_transfer_replace(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inodes: &[PersistedInode],
    allocator: &crate::allocation::BlockAllocator,
) -> io::Result<RecoveryReport> {
    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, allocator)?;
    store_inode_table(&mut capture, superblock, inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "whole-file transfer replacement did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "whole-file transfer replacement did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty(
        "whole-file transfer replacement rendered outside allocation and inode regions",
    )?;

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
            "whole-file transfer replacement recovery report is inconsistent",
        ));
    }
    Ok(report)
}

/// Atomically transfers the complete persisted block vector from one regular file to another,
/// emptying the source and releasing the destination's displaced blocks.
///
/// Source physical blocks keep their allocator ownership and are moved by reference into the
/// destination inode. Every block previously referenced by the destination is released from the
/// allocator in the same WAL transaction as both inode-table updates. Namespace state, inode
/// identities, and source block contents remain unchanged. If both files are empty, the operation is
/// a validated no-op.
///
/// Format v5 has no persisted byte length, so "whole file" means the complete sequence of persisted
/// 4 KiB logical blocks. This primitive does not define partial-final-block, sparse-hole, extent,
/// reflink/COW, or byte-EOF semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for identical, missing, or non-file inode endpoints. Returns
/// `InvalidData` if either inode contains duplicate physical references, the endpoints share a
/// physical block, or any referenced block disagrees with allocator ownership. Allocation, inode
/// encoding/capacity, WAL, checkpoint, and device errors are propagated.
pub fn transfer_replace_complete_file_blocks_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source_inode_id: u64,
    destination_inode_id: u64,
) -> io::Result<RecoveryReport> {
    if source_inode_id == destination_inode_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "whole-file transfer replacement requires distinct file inodes",
        ));
    }

    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let source_pos = inodes
        .iter()
        .position(|inode| inode.id == source_inode_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source inode is missing"))?;
    let destination_pos = inodes
        .iter()
        .position(|inode| inode.id == destination_inode_id)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination inode is missing")
        })?;

    if inodes[source_pos].kind != InodeKind::File || inodes[destination_pos].kind != InodeKind::File
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "whole-file transfer replacement endpoints must both be regular files",
        ));
    }

    let mut seen = HashSet::new();
    for block in inodes[source_pos]
        .blocks
        .iter()
        .chain(&inodes[destination_pos].blocks)
        .copied()
    {
        if !seen.insert(block) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "whole-file transfer replacement endpoints contain duplicate physical-block references",
            ));
        }
        if !allocator
            .is_owned(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "whole-file transfer replacement endpoint references a block that is not allocator-owned",
            ));
        }
    }

    if inodes[source_pos].blocks.is_empty() && inodes[destination_pos].blocks.is_empty() {
        return Ok(RecoveryReport::default());
    }

    let source_blocks = std::mem::take(&mut inodes[source_pos].blocks);
    let displaced = std::mem::replace(&mut inodes[destination_pos].blocks, source_blocks);
    for block in displaced {
        allocator
            .free(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }

    publish_whole_file_transfer_replace(device, superblock, &inodes, &allocator)
}
