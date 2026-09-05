use std::collections::HashSet;
use std::io;

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

/// Atomically transfers a contiguous logical-block range between two regular files.
///
/// The selected physical blocks are removed from `source_inode_id` and inserted at
/// `destination_index` in `destination_inode_id` without copying block contents or changing
/// allocator ownership. The complete inode-table update is published through one WAL transaction,
/// so recovery cannot complete with the blocks referenced by both files or by neither file.
/// Namespace metadata, block contents, allocation accounting, and on-disk format remain unchanged.
///
/// Format v5 has no persisted byte length, so this primitive is deliberately block-granular. It does
/// not claim byte-range move, EOF, sparse-hole, extent, reflink, or POSIX semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for identical source/destination inodes, a zero-length transfer, missing or
/// non-file inodes, a source range outside the source file, or a destination index beyond the current
/// destination block count. Returns `InvalidData` if a transferred physical block is not allocator-
/// owned or is already referenced by the destination. Journal-capacity, checkpoint, encoding, and
/// block-device I/O failures are propagated.
pub fn transfer_file_block_range_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source_inode_id: u64,
    source_index: usize,
    block_count: usize,
    destination_inode_id: u64,
    destination_index: usize,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    if source_inode_id == destination_inode_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "block-range transfer requires distinct source and destination inodes",
        ));
    }
    if block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "block-range transfer requires at least one logical block",
        ));
    }

    let allocator = load_allocator(device, superblock)?;
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
            "block-range transfer endpoints must both be regular files",
        ));
    }

    let source_end = source_index.checked_add(block_count).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "source logical range overflows",
        )
    })?;
    if source_end > inodes[source_pos].blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source logical range is beyond the end",
        ));
    }
    if destination_index > inodes[destination_pos].blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination logical index is beyond the end",
        ));
    }

    let moved = inodes[source_pos].blocks[source_index..source_end].to_vec();
    let destination_blocks = inodes[destination_pos]
        .blocks
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    for block in &moved {
        if !allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source block is not allocator-owned",
            ));
        }
        if destination_blocks.contains(block) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "source block is already referenced by destination inode",
            ));
        }
    }

    inodes[source_pos].blocks.drain(source_index..source_end);
    inodes[destination_pos]
        .blocks
        .splice(destination_index..destination_index, moved.iter().copied());

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_inode_table(&mut capture, superblock, &inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "block-range transfer image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("block-range transfer image rendered outside inode region")?;

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
            "block-range transfer recovery report is inconsistent",
        ));
    }

    Ok((moved, report))
}
