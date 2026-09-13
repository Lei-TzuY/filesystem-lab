use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::{BlockDevice, BLOCK_SIZE_U64};
use crate::format::Superblock;
use crate::fsck::check_device;
use crate::journal_checkpoint::recover_journal_and_checkpoint;

/// Recovered format-v5 block-space accounting suitable for statfs-like consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemSpace {
    pub block_size: u64,
    pub total_blocks: u64,
    pub reserved_blocks: u64,
    pub data_blocks: u64,
    pub allocated_data_blocks: u64,
    pub free_data_blocks: u64,
}

/// One contiguous run of allocator-free format-v5 data blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeSpaceExtent {
    pub start_block: u64,
    pub block_count: u64,
}

/// Recovered free-space topology derived from the durable allocator image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemFreeSpace {
    pub total_free_blocks: u64,
    pub largest_extent_blocks: u64,
    pub extents: Vec<FreeSpaceExtent>,
}

/// One bounded page of recovered free-space topology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilesystemFreeSpacePage {
    pub total_free_blocks: u64,
    pub largest_extent_blocks: u64,
    pub extents: Vec<FreeSpaceExtent>,
    pub next_after: Option<u64>,
}

/// Returns trustworthy block-space accounting from recovered durable filesystem state.
///
/// Any committed WAL transaction is recovered and checkpointed before accounting is observed. The
/// resulting home metadata is then checked with the repository-wide read-only fsck before space is
/// reported, so an allocation bitmap that disagrees with inode ownership is rejected instead of
/// advertising blocks as free incorrectly.
///
/// The report is intentionally limited to format-v5 logical-block accounting. It does not fabricate
/// byte-level capacity, quota, inode-capacity, sparse-file, or POSIX permission semantics that the
/// current on-disk format does not persist.
///
/// # Errors
///
/// Propagates recovery/checkpoint, device I/O, superblock/metadata decoding, journal validation, and
/// fsck ownership/namespace consistency failures. The supplied superblock must match the durable
/// filesystem image because recovery is performed before the read-only fsck pass.
pub fn filesystem_space(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
) -> io::Result<FilesystemSpace> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let report = check_device(device)?;
    validate_geometry(superblock, report.total_blocks, report.reserved_blocks)?;

    Ok(FilesystemSpace {
        block_size: BLOCK_SIZE_U64,
        total_blocks: report.total_blocks,
        reserved_blocks: report.reserved_blocks,
        data_blocks: report.data_blocks,
        allocated_data_blocks: report.allocated_blocks,
        free_data_blocks: report.free_blocks,
    })
}

/// Returns the exact contiguous free-data-block runs in recovered durable allocator state.
///
/// The query recovers and checkpoints committed WAL state, runs full read-only fsck, then scans the
/// durable allocation image from the first data block to the end of the filesystem. Adjacent free
/// blocks are coalesced into deterministic ascending extents. Reserved metadata blocks are never
/// reported. The sum of all extent lengths is checked against fsck's free-block accounting before a
/// result is returned, so allocator topology and repository-wide ownership accounting must agree.
///
/// This is an observation surface, not an allocation reservation or extent-format feature. It does
/// not mutate the filesystem and does not claim that a later allocation will receive a reported run.
/// The on-disk format remains filesystem format v5 with allocation-image version 1.
///
/// # Errors
///
/// Propagates recovery/checkpoint, device I/O, metadata decoding, journal validation, allocator, and
/// fsck consistency failures. Returns `InvalidData` if durable geometry changes during the query or
/// if scanned free-space topology disagrees with fsck accounting.
pub fn filesystem_free_space_extents(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
) -> io::Result<FilesystemFreeSpace> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let report = check_device(device)?;
    validate_geometry(superblock, report.total_blocks, report.reserved_blocks)?;
    let allocator = load_allocator(device, superblock)?;

    let mut extents = Vec::new();
    let mut run_start = None;
    let mut total_free_blocks = 0_u64;
    let mut largest_extent_blocks = 0_u64;

    for block in superblock.reserved_blocks()..superblock.total_blocks {
        let owned = allocator
            .is_owned(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        if !owned {
            total_free_blocks = total_free_blocks
                .checked_add(1)
                .ok_or_else(|| invalid_data("free-space extent accounting overflow"))?;
            if run_start.is_none() {
                run_start = Some(block);
            }
        } else if let Some(start_block) = run_start.take() {
            push_extent(&mut extents, &mut largest_extent_blocks, start_block, block)?;
        }
    }

    if let Some(start_block) = run_start {
        push_extent(
            &mut extents,
            &mut largest_extent_blocks,
            start_block,
            superblock.total_blocks,
        )?;
    }

    if total_free_blocks != report.free_blocks {
        return Err(invalid_data(
            "free-space extents disagree with fsck free-block accounting",
        ));
    }

    Ok(FilesystemFreeSpace {
        total_free_blocks,
        largest_extent_blocks,
        extents,
    })
}

/// Returns one bounded page of exact free-data-block runs from recovered durable allocator state.
///
/// `after_block` is an exclusive physical-block cursor. If it falls inside a free extent, the first
/// returned extent is clipped to start at the following block, so advancing pages never repeat free
/// blocks already consumed by the caller. A cursor before the data region advances to the first data
/// block, while a cursor at or beyond the filesystem end returns an empty page.
///
/// Each call independently recovers, fsck-validates, and scans one durable snapshot. The returned
/// extent vector is bounded by `limit`, but cursor pagination across concurrent mutations is not a
/// multi-call snapshot guarantee. Filesystem format remains v5; this API does not reserve free space
/// or introduce persistent extent allocation semantics.
///
/// # Errors
///
/// Returns `InvalidInput` when `limit` is zero. Recovery/checkpoint, fsck, allocator decoding, and
/// block-device failures are propagated.
pub fn filesystem_free_space_extents_page(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    after_block: Option<u64>,
    limit: usize,
) -> io::Result<FilesystemFreeSpacePage> {
    if limit == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "free-space extent page limit must be greater than zero",
        ));
    }

    let free_space = filesystem_free_space_extents(device, superblock)?;
    paginate_free_space_extents(free_space, superblock, after_block, limit)
}

fn paginate_free_space_extents(
    free_space: FilesystemFreeSpace,
    superblock: &Superblock,
    after_block: Option<u64>,
    limit: usize,
) -> io::Result<FilesystemFreeSpacePage> {
    if limit == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "free-space extent page limit must be greater than zero",
        ));
    }

    let first_block = after_block
        .and_then(|cursor| cursor.checked_add(1))
        .unwrap_or(superblock.reserved_blocks())
        .max(superblock.reserved_blocks());
    let mut eligible = Vec::new();

    for extent in free_space.extents {
        let extent_end = extent
            .start_block
            .checked_add(extent.block_count)
            .ok_or_else(|| invalid_data("free-space extent end overflow"))?;
        if extent_end <= first_block {
            continue;
        }
        if extent.start_block < first_block {
            eligible.push(FreeSpaceExtent {
                start_block: first_block,
                block_count: extent_end - first_block,
            });
        } else {
            eligible.push(extent);
        }
    }

    let has_more = eligible.len() > limit;
    eligible.truncate(limit);
    let next_after = if has_more {
        let last = eligible
            .last()
            .ok_or_else(|| invalid_data("free-space pagination lost its final extent"))?;
        last.start_block
            .checked_add(last.block_count)
            .and_then(|end| end.checked_sub(1))
    } else {
        None
    };

    Ok(FilesystemFreeSpacePage {
        total_free_blocks: free_space.total_free_blocks,
        largest_extent_blocks: free_space.largest_extent_blocks,
        extents: eligible,
        next_after,
    })
}

fn validate_geometry(
    superblock: &Superblock,
    total_blocks: u64,
    reserved_blocks: u64,
) -> io::Result<()> {
    if total_blocks != superblock.total_blocks || reserved_blocks != superblock.reserved_blocks() {
        return Err(invalid_data(
            "durable superblock geometry changed during filesystem-space query",
        ));
    }
    Ok(())
}

fn push_extent(
    extents: &mut Vec<FreeSpaceExtent>,
    largest_extent_blocks: &mut u64,
    start_block: u64,
    end_block: u64,
) -> io::Result<()> {
    let block_count = end_block
        .checked_sub(start_block)
        .filter(|count| *count > 0)
        .ok_or_else(|| invalid_data("invalid free-space extent bounds"))?;
    *largest_extent_blocks = (*largest_extent_blocks).max(block_count);
    extents.push(FreeSpaceExtent {
        start_block,
        block_count,
    });
    Ok(())
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
