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
    if inode.blocks.is_empty() {
        return Ok(Vec::new());
    }

    let mut extents = Vec::new();
    let mut logical_start = 0_usize;
    let mut physical_start = inode.blocks[0];
    let mut previous = physical_start;

    for (logical_index, physical_block) in inode.blocks.iter().copied().enumerate().skip(1) {
        if previous.checked_add(1) == Some(physical_block) {
            previous = physical_block;
            continue;
        }

        push_extent(
            &mut extents,
            logical_start,
            physical_start,
            logical_index,
        )?;
        logical_start = logical_index;
        physical_start = physical_block;
        previous = physical_block;
    }

    push_extent(
        &mut extents,
        logical_start,
        physical_start,
        inode.blocks.len(),
    )?;
    Ok(extents)
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
