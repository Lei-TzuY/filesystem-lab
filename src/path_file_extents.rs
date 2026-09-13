use std::io;

use crate::block::BlockDevice;
use crate::format::Superblock;
use crate::fsck::check_device;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;

/// One maximal run of logically adjacent blocks backed by physically adjacent blocks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileExtent {
    pub logical_start: usize,
    pub physical_start: u64,
    pub block_count: usize,
}

/// One bounded page of recovered physical block runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileExtentPage {
    pub extents: Vec<FileExtent>,
    /// Exclusive logical-block cursor for a subsequent page, present only when more extents remain.
    pub next_after_logical: Option<usize>,
}

/// Reports the recovered physical block runs backing a regular file named by pathname.
///
/// Any committed WAL is recovered and checkpointed before pathname resolution. The repository-wide
/// read-only fsck pass must then accept allocator ownership, inode references, and namespace state
/// before the file mapping is exposed. Intermediate and final symbolic links are followed.
///
/// Format v5 persists an explicit physical block number for every logical file block. This query
/// coalesces only adjacent logical entries whose physical block numbers are also consecutive. It is
/// therefore an observation of the current explicit mapping, not a persistent extent record, sparse
/// mapping, allocation reservation, or promise that later mutations will preserve contiguity.
/// A zero-block regular file returns an empty vector.
///
/// # Errors
///
/// Propagates recovery/checkpoint, pathname resolution, device I/O, metadata decoding, and fsck
/// consistency failures. Returns `InvalidInput` when the resolved pathname is not a regular file and
/// `InvalidData` if the resolved inode disappears from the validated inode table or an extent length
/// cannot be represented.
pub fn file_extents_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<Vec<FileExtent>> {
    let blocks = recovered_file_blocks(device, superblock, path)?;
    coalesce_extents(&blocks)
}

/// Reports one bounded page of recovered physical block runs backing a regular file.
///
/// `after_logical` is an exclusive logical-block cursor. An extent is eligible when its logical end
/// lies after the cursor. If the cursor falls inside an extent, the returned first extent is clipped
/// to begin immediately after the cursor, so every logical block remains observable exactly once
/// across advancing pages. The cursor need not coincide with an extent boundary.
///
/// Each call independently recovers, fsck-validates, and observes one durable snapshot; pagination
/// across concurrent file mutations is not a multi-call snapshot guarantee. Format v5 remains an
/// explicit block-vector format and this API does not create persistent extent records.
///
/// # Errors
///
/// Returns `InvalidInput` when `limit` is zero or the resolved pathname is not a regular file.
/// Recovery/checkpoint, fsck, pathname-resolution, and metadata failures are propagated.
pub fn file_extents_page_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    after_logical: Option<usize>,
    limit: usize,
) -> io::Result<FileExtentPage> {
    if limit == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pathname extent page limit must be greater than zero",
        ));
    }
    let blocks = recovered_file_blocks(device, superblock, path)?;
    paginate_extents(coalesce_extents(&blocks)?, after_logical, limit)
}

fn recovered_file_blocks(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<Vec<u64>> {
    recover_journal_and_checkpoint(device, *superblock)?;
    check_device(device)?;

    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    let inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| invalid_data("resolved inode is missing from validated inode table"))?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resolved pathname is not a regular file",
        ));
    }
    Ok(inode.blocks.clone())
}

fn coalesce_extents(blocks: &[u64]) -> io::Result<Vec<FileExtent>> {
    if blocks.is_empty() {
        return Ok(Vec::new());
    }

    let mut extents = Vec::new();
    let mut logical_start = 0_usize;
    let mut physical_start = blocks[0];
    let mut previous = physical_start;

    for (logical_index, physical_block) in blocks.iter().copied().enumerate().skip(1) {
        if previous.checked_add(1) == Some(physical_block) {
            previous = physical_block;
            continue;
        }

        push_extent(&mut extents, logical_start, physical_start, logical_index)?;
        logical_start = logical_index;
        physical_start = physical_block;
        previous = physical_block;
    }

    push_extent(&mut extents, logical_start, physical_start, blocks.len())?;
    Ok(extents)
}

fn paginate_extents(
    extents: Vec<FileExtent>,
    after_logical: Option<usize>,
    limit: usize,
) -> io::Result<FileExtentPage> {
    if limit == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pathname extent page limit must be greater than zero",
        ));
    }

    let first_logical = after_logical.and_then(|cursor| cursor.checked_add(1)).unwrap_or(0);
    let mut eligible = Vec::new();
    for extent in extents {
        let logical_end = extent
            .logical_start
            .checked_add(extent.block_count)
            .ok_or_else(|| invalid_data("file extent logical end overflow"))?;
        if logical_end <= first_logical {
            continue;
        }
        if extent.logical_start < first_logical {
            let skipped = first_logical - extent.logical_start;
            let skipped_u64 = u64::try_from(skipped)
                .map_err(|_| invalid_data("file extent cursor offset cannot be represented"))?;
            eligible.push(FileExtent {
                logical_start: first_logical,
                physical_start: extent
                    .physical_start
                    .checked_add(skipped_u64)
                    .ok_or_else(|| invalid_data("file extent physical cursor overflow"))?,
                block_count: extent.block_count - skipped,
            });
        } else {
            eligible.push(extent);
        }
    }

    let has_more = eligible.len() > limit;
    eligible.truncate(limit);
    let next_after_logical = if has_more {
        let last = eligible
            .last()
            .ok_or_else(|| invalid_data("extent pagination lost its final entry"))?;
        last.logical_start
            .checked_add(last.block_count)
            .and_then(|end| end.checked_sub(1))
    } else {
        None
    };

    Ok(FileExtentPage {
        extents: eligible,
        next_after_logical,
    })
}

fn push_extent(
    extents: &mut Vec<FileExtent>,
    logical_start: usize,
    physical_start: u64,
    logical_end: usize,
) -> io::Result<()> {
    let block_count = logical_end
        .checked_sub(logical_start)
        .filter(|count| *count > 0)
        .ok_or_else(|| invalid_data("invalid file extent bounds"))?;
    extents.push(FileExtent {
        logical_start,
        physical_start,
        block_count,
    });
    Ok(())
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paginates_fragmented_mapping_by_extent_count() {
        let extents = coalesce_extents(&[20, 21, 40, 60, 61]).unwrap();
        let first = paginate_extents(extents.clone(), None, 2).unwrap();
        assert_eq!(
            first,
            FileExtentPage {
                extents: vec![
                    FileExtent { logical_start: 0, physical_start: 20, block_count: 2 },
                    FileExtent { logical_start: 2, physical_start: 40, block_count: 1 },
                ],
                next_after_logical: Some(2),
            }
        );
        assert_eq!(
            paginate_extents(extents, first.next_after_logical, 2).unwrap(),
            FileExtentPage {
                extents: vec![FileExtent { logical_start: 3, physical_start: 60, block_count: 2 }],
                next_after_logical: None,
            }
        );
    }

    #[test]
    fn cursor_inside_extent_clips_without_repeating_blocks() {
        let extents = vec![FileExtent { logical_start: 4, physical_start: 100, block_count: 5 }];
        assert_eq!(
            paginate_extents(extents, Some(5), 1).unwrap(),
            FileExtentPage {
                extents: vec![FileExtent { logical_start: 6, physical_start: 102, block_count: 3 }],
                next_after_logical: None,
            }
        );
    }

    #[test]
    fn stale_cursor_between_extents_advances_to_next_run() {
        let extents = vec![
            FileExtent { logical_start: 0, physical_start: 10, block_count: 2 },
            FileExtent { logical_start: 4, physical_start: 30, block_count: 2 },
        ];
        assert_eq!(
            paginate_extents(extents, Some(2), 1).unwrap().extents,
            vec![FileExtent { logical_start: 4, physical_start: 30, block_count: 2 }]
        );
    }

    #[test]
    fn rejects_zero_page_limit() {
        assert_eq!(
            paginate_extents(Vec::new(), None, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
