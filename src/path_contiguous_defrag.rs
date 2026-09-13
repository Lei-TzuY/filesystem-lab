use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_replace_contiguous::replace_file_blocks_contiguous_journaled;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Relocates every block of one non-empty regular file into one contiguous physical run.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. Intermediate
/// and final symbolic links use the repository-wide bounded resolver. If the file is already
/// physically contiguous, the operation is an idempotent no-op. Otherwise all current data images
/// are snapshotted before delegating to the contiguous replacement transaction, which reserves one
/// lowest-address first-fit run before releasing the old blocks and atomically publishes allocator
/// ownership, the inode mapping, and copied data images.
///
/// Format v5 continues to persist an explicit block vector. This operation changes physical layout
/// only; it does not change namespace state, logical block order, file contents, the on-disk format,
/// byte-level EOF semantics, or create a persistent extent record.
///
/// # Errors
///
/// Propagates recovery/checkpoint and pathname lookup failures. Returns `InvalidInput` for an empty
/// file or a resolved non-file inode, and when fragmentation or exhaustion prevents reserving a run
/// as large as the file. Returns `InvalidData` when allocator ownership disagrees with an inode block
/// reference. Journal-capacity and block-device I/O failures are propagated.
pub fn defragment_file_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;

    let allocator = load_allocator(device, superblock)?;
    let inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "defragment target inode is missing",
            )
        })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "defragment target must be a regular file",
        ));
    }
    if inode.blocks.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "defragment target must contain at least one logical block",
        ));
    }

    for block in &inode.blocks {
        if !allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "defragment target block is not allocator-owned",
            ));
        }
    }

    if inode.blocks.windows(2).all(|pair| pair[1] == pair[0] + 1) {
        return Ok((inode.blocks.clone(), RecoveryReport::default()));
    }

    let old_blocks = inode.blocks.clone();
    let mut snapshots = Vec::with_capacity(old_blocks.len());
    for block in &old_blocks {
        let mut image = [0_u8; BLOCK_SIZE];
        device.read_block(*block, &mut image)?;
        snapshots.push(image);
    }

    let (new_blocks, _displaced, report) = replace_file_blocks_contiguous_journaled(
        device,
        superblock,
        inode_id,
        0,
        old_blocks.len(),
        &snapshots,
    )?;
    Ok((new_blocks, report))
}

/// Relocates one non-empty logical-block range of a regular file into one contiguous physical run.
///
/// The selected range preserves its logical position, length, and data. Blocks before and after the
/// range keep their existing mappings. Older committed WAL state is recovered and checkpointed first,
/// and the pathname follows intermediate and final symbolic links through the bounded resolver.
///
/// If the selected physical blocks are already contiguous, the operation is an idempotent no-op.
/// Otherwise the current data images are snapshotted before the existing contiguous replacement WAL
/// path reserves a fresh lowest-address run, releases the displaced ownership, and atomically publishes
/// allocation metadata, the updated inode vector, and copied data images.
///
/// Format v5 continues to persist an explicit block vector; range contiguity is an allocation-time
/// result only and does not create a persistent extent record.
///
/// # Errors
///
/// Returns `InvalidInput` for a zero block count, range overflow, a range outside the file, a missing
/// inode, or a non-file target. Returns `InvalidData` when allocator ownership disagrees with a selected
/// inode block. Contiguous-space exhaustion, journal-capacity, recovery, checkpoint, and device I/O
/// failures are propagated.
pub fn defragment_file_range_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    start: usize,
    block_count: usize,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    if block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "defragment range must contain at least one logical block",
        ));
    }
    let end = start.checked_add(block_count).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "defragment range overflows usize",
        )
    })?;

    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;

    let allocator = load_allocator(device, superblock)?;
    let inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "defragment range target inode is missing",
            )
        })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "defragment range target must be a regular file",
        ));
    }
    if end > inode.blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "defragment range exceeds existing logical blocks",
        ));
    }

    let selected = &inode.blocks[start..end];
    for block in selected {
        if !allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "defragment range block is not allocator-owned",
            ));
        }
    }

    if selected.windows(2).all(|pair| pair[1] == pair[0] + 1) {
        return Ok((selected.to_vec(), RecoveryReport::default()));
    }

    let old_blocks = selected.to_vec();
    let mut snapshots = Vec::with_capacity(old_blocks.len());
    for block in &old_blocks {
        let mut image = [0_u8; BLOCK_SIZE];
        device.read_block(*block, &mut image)?;
        snapshots.push(image);
    }

    let (new_blocks, _displaced, report) = replace_file_blocks_contiguous_journaled(
        device,
        superblock,
        inode_id,
        start,
        block_count,
        &snapshots,
    )?;
    Ok((new_blocks, report))
}
