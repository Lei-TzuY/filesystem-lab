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

fn snapshot_owned_blocks(
    device: &mut impl BlockDevice,
    allocator: &BlockAllocator,
    blocks: &[u64],
    message: &'static str,
) -> io::Result<Vec<[u8; BLOCK_SIZE]>> {
    let mut snapshots = Vec::with_capacity(blocks.len());
    for block in blocks {
        if !allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, message));
        }
        let mut image = [0_u8; BLOCK_SIZE];
        device.read_block(*block, &mut image)?;
        snapshots.push(image);
    }
    Ok(snapshots)
}

fn publish_replacement(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    allocator: &BlockAllocator,
    inodes: &[PersistedInode],
    new_blocks: &[u64],
    snapshots: &[[u8; BLOCK_SIZE]],
) -> io::Result<RecoveryReport> {
    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, allocator)?;
    store_inode_table(&mut capture, superblock, inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "clone replacement image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "clone replacement image did not render every inode metadata block",
        &mut changed,
    )?;
    capture
        .ensure_empty("clone replacement image rendered outside allocation and inode regions")?;
    changed.extend(new_blocks.iter().copied().zip(snapshots.iter().copied()));

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
            "clone replacement recovery report is inconsistent",
        ));
    }
    Ok(report)
}

/// Replaces an existing destination logical-block range with freshly allocated source clones.
///
/// The source images are snapshotted first. Fresh physical blocks are allocated while every
/// displaced destination block is still owned, guaranteeing that replacement blocks are distinct
/// from the old destination homes. The displaced blocks are then released and allocator ownership,
/// destination inode references, and cloned data homes are published through one WAL transaction.
/// Source and destination must be distinct regular-file inodes. File block counts, namespace state,
/// source mappings/data, and unaffected destination ordering are preserved.
///
/// Format v5 has no persisted byte length, so this operation is deliberately block-granular. It does
/// not define EOF, partial-block replacement, sparse holes, extents, reflinks, or POSIX semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty range, identical endpoints, missing/non-file inodes, ranges
/// outside existing logical blocks, or insufficient free blocks. Returns `InvalidData` when
/// allocator ownership disagrees with a selected source or destination reference. Journal-capacity,
/// encoding, recovery, checkpoint, and block-device I/O failures are propagated.
pub fn clone_file_blocks_replace_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source_inode: u64,
    source_start: usize,
    block_count: usize,
    destination_inode: u64,
    destination_start: usize,
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    if block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone replacement requires at least one source block",
        ));
    }
    if source_inode == destination_inode {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone replacement requires distinct source and destination inodes",
        ));
    }

    let source_end = source_start.checked_add(block_count).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone replacement source range overflows usize",
        )
    })?;
    let destination_end = destination_start.checked_add(block_count).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone replacement destination range overflows usize",
        )
    })?;

    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let source_index = inodes
        .iter()
        .position(|inode| inode.id == source_inode)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "clone source inode is missing")
        })?;
    let destination_index = inodes
        .iter()
        .position(|inode| inode.id == destination_inode)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "clone destination inode is missing",
            )
        })?;
    if inodes[source_index].kind != InodeKind::File
        || inodes[destination_index].kind != InodeKind::File
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone source and destination must be regular files",
        ));
    }
    if source_end > inodes[source_index].blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone source range exceeds existing logical blocks",
        ));
    }
    if destination_end > inodes[destination_index].blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone destination range exceeds existing logical blocks",
        ));
    }

    let source_blocks = inodes[source_index].blocks[source_start..source_end].to_vec();
    let displaced_blocks =
        inodes[destination_index].blocks[destination_start..destination_end].to_vec();
    let snapshots = snapshot_owned_blocks(
        device,
        &allocator,
        &source_blocks,
        "clone source block is not allocator-owned",
    )?;
    let _ = snapshot_owned_blocks(
        device,
        &allocator,
        &displaced_blocks,
        "clone destination block is not allocator-owned",
    )?;

    let mut new_blocks = Vec::with_capacity(block_count);
    for _ in 0..block_count {
        let block = allocator
            .allocate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        new_blocks.push(block);
    }
    for block in &displaced_blocks {
        allocator
            .free(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }
    inodes[destination_index].blocks[destination_start..destination_end]
        .copy_from_slice(&new_blocks);

    let report = publish_replacement(
        device,
        superblock,
        &allocator,
        &inodes,
        &new_blocks,
        &snapshots,
    )?;
    Ok((new_blocks, displaced_blocks, report))
}
