use std::io;

use crate::allocation::BlockAllocator;
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
    let scan = scan_free_space(&allocator, superblock, superblock.reserved_blocks(), None)?;

    if scan.total_free_blocks != report.free_blocks {
        return Err(invalid_data(
            "free-space extents disagree with fsck free-block accounting",
        ));
    }

    Ok(FilesystemFreeSpace {
        total_free_blocks: scan.total_free_blocks,
        largest_extent_blocks: scan.largest_extent_blocks,
        extents: scan.extents,
    })
}

/// Returns one bounded page of exact free-data-block runs from recovered durable allocator state.
///
/// `after_block` is an exclusive physical-block cursor. A cursor before the data region is clipped to
/// the first data block. A cursor at or beyond the end of the filesystem returns an empty page. If a
/// cursor lands inside a free extent, the first returned extent begins at the next block so pages
/// never repeat already-consumed free blocks. `next_after` is the last physical block of the final
/// returned extent and is present only when another free extent remains after the page.
///
/// The query still scans the complete durable allocation image so `total_free_blocks` and
/// `largest_extent_blocks` describe the whole recovered filesystem and are checked against full
/// read-only fsck accounting. Only the returned extent vector is bounded by `limit`; the operation
/// does not reserve space or promise future allocation placement. Filesystem format remains v5.
///
/// # Errors
///
/// Returns `InvalidInput` when `limit` is zero. Propagates recovery/checkpoint, device I/O, metadata
/// decoding, journal validation, allocator, and fsck consistency failures. Returns `InvalidData` if
/// durable geometry changes during the query or allocator topology disagrees with fsck accounting.
pub fn filesystem_free_space_extents_page(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    after_block: Option<u64>,
    limit: usize,
) -> io::Result<FilesystemFreeSpacePage> {
    if limit == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "free-space extent page limit must be non-zero",
        ));
    }

    recover_journal_and_checkpoint(device, *superblock)?;
    let report = check_device(device)?;
    validate_geometry(superblock, report.total_blocks, report.reserved_blocks)?;
    let allocator = load_allocator(device, superblock)?;
    let page_start = after_block
        .and_then(|block| block.checked_add(1))
        .unwrap_or_else(|| {
            if after_block.is_some() {
                superblock.total_blocks
            } else {
                superblock.reserved_blocks()
            }
        })
        .max(superblock.reserved_blocks())
        .min(superblock.total_blocks);
    let scan = scan_free_space(&allocator, superblock, page_start, Some(limit))?;

    if scan.total_free_blocks != report.free_blocks {
        return Err(invalid_data(
            "free-space extents disagree with fsck free-block accounting",
        ));
    }

    let next_after = if scan.has_more {
        scan.extents.last().map(extent_last_block).transpose()?
    } else {
        None
    };

    Ok(FilesystemFreeSpacePage {
        total_free_blocks: scan.total_free_blocks,
        largest_extent_blocks: scan.largest_extent_blocks,
        extents: scan.extents,
        next_after,
    })
}

#[derive(Debug)]
struct FreeSpaceScan {
    total_free_blocks: u64,
    largest_extent_blocks: u64,
    extents: Vec<FreeSpaceExtent>,
    has_more: bool,
}

fn scan_free_space(
    allocator: &BlockAllocator,
    superblock: &Superblock,
    page_start: u64,
    limit: Option<usize>,
) -> io::Result<FreeSpaceScan> {
    let mut extents = Vec::new();
    let mut run_start = None;
    let mut total_free_blocks = 0_u64;
    let mut largest_extent_blocks = 0_u64;
    let mut has_more = false;

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
            record_extent(
                &mut extents,
                &mut largest_extent_blocks,
                &mut has_more,
                start_block,
                block,
                page_start,
                limit,
            )?;
        }
    }

    if let Some(start_block) = run_start {
        record_extent(
            &mut extents,
            &mut largest_extent_blocks,
            &mut has_more,
            start_block,
            superblock.total_blocks,
            page_start,
            limit,
        )?;
    }

    Ok(FreeSpaceScan {
        total_free_blocks,
        largest_extent_blocks,
        extents,
        has_more,
    })
}

#[allow(clippy::too_many_arguments)]
fn record_extent(
    extents: &mut Vec<FreeSpaceExtent>,
    largest_extent_blocks: &mut u64,
    has_more: &mut bool,
    start_block: u64,
    end_block: u64,
    page_start: u64,
    limit: Option<usize>,
) -> io::Result<()> {
    let block_count = end_block
        .checked_sub(start_block)
        .filter(|count| *count > 0)
        .ok_or_else(|| invalid_data("invalid free-space extent bounds"))?;
    *largest_extent_blocks = (*largest_extent_blocks).max(block_count);

    let clipped_start = start_block.max(page_start);
    if clipped_start >= end_block {
        return Ok(());
    }
    let clipped_count = end_block
        .checked_sub(clipped_start)
        .filter(|count| *count > 0)
        .ok_or_else(|| invalid_data("invalid clipped free-space extent bounds"))?;

    if limit.is_some_and(|value| extents.len() >= value) {
        *has_more = true;
        return Ok(());
    }

    extents.push(FreeSpaceExtent {
        start_block: clipped_start,
        block_count: clipped_count,
    });
    Ok(())
}

fn extent_last_block(extent: &FreeSpaceExtent) -> io::Result<u64> {
    extent
        .start_block
        .checked_add(extent.block_count)
        .and_then(|end| end.checked_sub(1))
        .ok_or_else(|| invalid_data("free-space extent end overflow"))
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

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
