use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_range_read::read_file_range;
use crate::file_replace_contiguous::replace_file_blocks_contiguous_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_clone_replace::PathCloneReplaceRange;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically replaces an existing destination logical-block range with independent source copies
/// whose fresh physical homes form one contiguous first-fit run.
///
/// Any older committed WAL is recovered and checkpointed before resolving either pathname. Source
/// bytes are snapshotted from the resolved regular-file inode before destination mutation. The
/// destination replacement then delegates to [`replace_file_blocks_contiguous_journaled`], which
/// reserves all fresh blocks before releasing displaced ownership and publishes allocator metadata,
/// the destination inode block vector, and copied data images in one WAL transaction.
///
/// Source and destination must resolve to distinct inodes. Format v5 continues to persist explicit
/// inode block vectors; contiguity is an allocation-time property of the fresh clone blocks, not a
/// persistent extent, reflink/COW, sparse-hole, byte-EOF, or broader POSIX guarantee.
///
/// # Errors
///
/// Propagates recovery/checkpoint, bounded pathname resolution, source range-read, contiguous
/// allocation/replacement, journal-capacity, and durable I/O failures. Returns `InvalidInput` for an
/// empty source range, identical resolved endpoints, invalid/non-file endpoints, out-of-range logical
/// blocks, arithmetic overflow, or lack of a sufficiently large contiguous free run.
pub fn clone_file_blocks_contiguous_replace_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: PathCloneReplaceRange<'_>,
    destination_path: &str,
    destination_start: usize,
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    if source.block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous clone replacement requires at least one source block",
        ));
    }

    recover_journal_and_checkpoint(device, *superblock)?;
    let source_inode = resolve_path_following_symlinks(device, superblock, source.path)?;
    let destination_inode = resolve_path_following_symlinks(device, superblock, destination_path)?;
    if source_inode == destination_inode {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous clone replacement requires distinct source and destination inodes",
        ));
    }

    let byte_len = source
        .block_count
        .checked_mul(BLOCK_SIZE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "clone byte length overflow"))?;
    let snapshot = read_file_range(
        device,
        superblock,
        source_inode,
        source.start,
        0,
        byte_len,
    )?;
    let mut blocks = Vec::with_capacity(source.block_count);
    for chunk in snapshot.chunks_exact(BLOCK_SIZE) {
        let mut image = [0_u8; BLOCK_SIZE];
        image.copy_from_slice(chunk);
        blocks.push(image);
    }
    debug_assert_eq!(blocks.len(), source.block_count);

    replace_file_blocks_contiguous_journaled(
        device,
        superblock,
        destination_inode,
        destination_start,
        source.block_count,
        &blocks,
    )
}
