use std::{collections::HashSet, io};

use crate::allocation::BlockAllocator;
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
    let left_end = left_index
        .checked_add(block_count)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "left range overflows"))?;
    let right_end = right_index
        .checked_add(block_count)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "right range overflows"))?;
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

    publish_exchange_inodes(device, superblock, &inodes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileBlockExchangeRange {
    pub inode: u64,
    pub start: usize,
    pub block_count: usize,
}

fn exchange_range_end(range: FileBlockExchangeRange, label: &'static str) -> io::Result<usize> {
    if range.block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "variable block-range exchange requires non-empty ranges",
        ));
    }
    range.start.checked_add(range.block_count).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{label} exchange range overflows usize"),
        )
    })
}

fn exchange_inode_index(
    inodes: &[PersistedInode],
    inode_id: u64,
    label: &'static str,
) -> io::Result<usize> {
    inodes
        .iter()
        .position(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{label} exchange inode is missing"),
            )
        })
}

fn validate_variable_exchange_ranges(
    inodes: &[PersistedInode],
    left: FileBlockExchangeRange,
    right: FileBlockExchangeRange,
) -> io::Result<(usize, usize, usize, usize)> {
    if left.inode == right.inode {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "variable block-range exchange requires distinct files",
        ));
    }
    let left_end = exchange_range_end(left, "left")?;
    let right_end = exchange_range_end(right, "right")?;
    let left_pos = exchange_inode_index(inodes, left.inode, "left")?;
    let right_pos = exchange_inode_index(inodes, right.inode, "right")?;
    if inodes[left_pos].kind != InodeKind::File || inodes[right_pos].kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "variable block-range exchange endpoints must be regular files",
        ));
    }
    if left_end > inodes[left_pos].blocks.len() || right_end > inodes[right_pos].blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "variable block-range exchange is beyond file end",
        ));
    }
    Ok((left_pos, left_end, right_pos, right_end))
}

fn validate_exchange_ownership(
    allocator: &BlockAllocator,
    left: &PersistedInode,
    right: &PersistedInode,
) -> io::Result<()> {
    let mut seen = HashSet::new();
    for block in left.blocks.iter().chain(&right.blocks).copied() {
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
    Ok(())
}

fn validate_single_exchange_ownership(
    allocator: &BlockAllocator,
    inode: &PersistedInode,
) -> io::Result<()> {
    let mut seen = HashSet::new();
    for block in inode.blocks.iter().copied() {
        if !seen.insert(block) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "same-file exchange inode contains duplicate physical-block references",
            ));
        }
        if !allocator
            .is_owned(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "same-file exchange references a block that is not allocator-owned",
            ));
        }
    }
    Ok(())
}

fn publish_exchange_inodes(
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

/// Atomically exchanges differently sized logical-block ranges between two regular files.
///
/// The selected physical references trade ownership between the two inode block vectors without
/// allocation, freeing, or data copying. The union of referenced physical blocks therefore remains
/// unchanged, while either file may gain or lose logical blocks. Namespace and inode identities are
/// untouched. The complete resized inode-table image is validated before WAL publication.
///
/// Format v5 has no persisted byte length, so this operation is deliberately block-granular and does
/// not define EOF, partial-block, sparse-hole, extent, reflink, or POSIX range-exchange semantics.
///
/// # Errors
/// Returns `InvalidInput` for identical/missing/non-file endpoints, empty or overflowing ranges, or
/// ranges beyond existing logical blocks. Returns `InvalidData` for duplicate physical references or
/// allocator ownership disagreement. Inode encoding/capacity, WAL, checkpoint, and device errors are
/// propagated before or during publication as appropriate.
pub fn exchange_variable_file_block_ranges_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    left: FileBlockExchangeRange,
    right: FileBlockExchangeRange,
) -> io::Result<RecoveryReport> {
    let allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let (left_pos, left_end, right_pos, right_end) =
        validate_variable_exchange_ranges(&inodes, left, right)?;
    validate_exchange_ownership(&allocator, &inodes[left_pos], &inodes[right_pos])?;

    let left_blocks = inodes[left_pos].blocks[left.start..left_end].to_vec();
    let right_blocks = inodes[right_pos].blocks[right.start..right_end].to_vec();
    inodes[left_pos]
        .blocks
        .splice(left.start..left_end, right_blocks.iter().copied());
    inodes[right_pos]
        .blocks
        .splice(right.start..right_end, left_blocks.iter().copied());

    publish_exchange_inodes(device, superblock, &inodes)
}

/// Atomically exchanges two disjoint logical-block ranges within one regular file.
///
/// Range coordinates refer to the original block vector. The ranges may have different lengths but
/// must not overlap. Physical blocks are only reordered: no allocation, freeing, data writes, or
/// namespace updates occur, so allocator accounting and inode identity remain unchanged.
///
/// Format v5 has no persisted byte length, so this operation is block-granular and does not define
/// EOF, sparse-hole, extent, reflink, or POSIX range-exchange semantics.
///
/// # Errors
/// Returns `InvalidInput` for different/missing/non-file inode endpoints, empty, overlapping,
/// overflowing, or out-of-range intervals. Returns `InvalidData` for duplicate physical references
/// or allocator ownership disagreement. WAL, checkpoint, codec, and device errors are propagated.
pub fn exchange_same_file_block_ranges_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first: FileBlockExchangeRange,
    second: FileBlockExchangeRange,
) -> io::Result<RecoveryReport> {
    if first.inode != second.inode {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "same-file block-range exchange requires one inode",
        ));
    }
    let first_end = exchange_range_end(first, "first")?;
    let second_end = exchange_range_end(second, "second")?;
    let (earlier, earlier_end, later, later_end) = if first.start <= second.start {
        (first, first_end, second, second_end)
    } else {
        (second, second_end, first, first_end)
    };
    if earlier_end > later.start {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "same-file exchange ranges must be disjoint",
        ));
    }

    let allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let inode_pos = exchange_inode_index(&inodes, first.inode, "same-file")?;
    if inodes[inode_pos].kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "same-file exchange endpoint must be a regular file",
        ));
    }
    if later_end > inodes[inode_pos].blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "same-file exchange is beyond file end",
        ));
    }
    validate_single_exchange_ownership(&allocator, &inodes[inode_pos])?;

    let old = inodes[inode_pos].blocks.clone();
    let mut reordered = Vec::with_capacity(old.len());
    reordered.extend_from_slice(&old[..earlier.start]);
    reordered.extend_from_slice(&old[later.start..later_end]);
    reordered.extend_from_slice(&old[earlier_end..later.start]);
    reordered.extend_from_slice(&old[earlier.start..earlier_end]);
    reordered.extend_from_slice(&old[later_end..]);
    inodes[inode_pos].blocks = reordered;

    publish_exchange_inodes(device, superblock, &inodes)
}
