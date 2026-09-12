use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_range_read::read_file_range;
use crate::file_replace_contiguous::replace_file_blocks_contiguous_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_clone_splice::PathCloneSpliceRange;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically replaces a non-empty destination logical-block range with a differently sized source
/// clone whose fresh physical homes form one contiguous first-fit run.
///
/// Any older committed WAL is recovered and checkpointed before either pathname is resolved. The
/// source complete-block range is snapshotted before destination mutation, then the destination is
/// rewritten through [`replace_file_blocks_contiguous_journaled`]. That existing transaction path
/// reserves every fresh block before releasing displaced ownership and publishes allocation metadata,
/// the resized destination inode block vector, and copied data images in one WAL transaction.
///
/// Source and destination must resolve to distinct regular-file inodes. Format v5 continues to store
/// explicit inode block vectors; contiguity is an allocation-time property of the fresh clone run, not
/// a persistent extent, reflink/COW, sparse-hole, byte-EOF, or broader POSIX guarantee.
///
/// # Errors
///
/// Propagates recovery/checkpoint, bounded pathname resolution, source range-read, contiguous
/// allocation/replacement, journal-capacity, and durable I/O failures. Returns `InvalidInput` for an
/// empty source or destination range, identical resolved endpoints, invalid/non-file endpoints,
/// out-of-range logical blocks, arithmetic overflow, or lack of a sufficiently large contiguous free
/// run.
pub fn clone_file_blocks_contiguous_splice_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: PathCloneSpliceRange<'_>,
    destination: PathCloneSpliceRange<'_>,
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    if source.block_count == 0 || destination.block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous clone splice requires non-empty source and destination ranges",
        ));
    }

    recover_journal_and_checkpoint(device, *superblock)?;
    let source_inode = resolve_path_following_symlinks(device, superblock, source.path)?;
    let destination_inode = resolve_path_following_symlinks(device, superblock, destination.path)?;
    if source_inode == destination_inode {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous clone splice requires distinct source and destination inodes",
        ));
    }

    let byte_len = source
        .block_count
        .checked_mul(BLOCK_SIZE)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "clone byte length overflow"))?;
    let snapshot = read_file_range(device, superblock, source_inode, source.start, 0, byte_len)?;
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
        destination.start,
        destination.block_count,
        &blocks,
    )
}
