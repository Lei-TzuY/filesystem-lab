use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_append_contiguous::append_file_blocks_contiguous_journaled;
use crate::file_range_read::read_file_range;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_clone_append::PathCloneAppendRange;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically appends independent copies of a source logical-block range to a destination file,
/// placing every fresh physical block in one contiguous first-fit run.
///
/// Any older committed WAL is recovered and checkpointed before either pathname is resolved. Source
/// bytes are snapshotted before destination mutation, so source and destination may resolve to the
/// same regular-file inode without the append changing the data being cloned. The copied blocks are
/// then delegated to [`append_file_blocks_contiguous_journaled`], which publishes allocator
/// ownership, destination inode growth, and copied data images in one WAL transaction.
///
/// Format v5 continues to persist explicit inode block vectors. Contiguity is an allocation-time
/// property of the fresh clone blocks, not a persistent extent, reflink/COW, sparse-hole, byte-EOF,
/// or broader POSIX guarantee.
///
/// # Errors
///
/// Propagates recovery/checkpoint, bounded pathname resolution, source range-read, contiguous
/// append, journal-capacity, and durable I/O failures. Returns `InvalidInput` for an empty source
/// range, invalid/non-file endpoints, an out-of-range source interval, arithmetic overflow, or lack
/// of a sufficiently large contiguous free run.
pub fn clone_file_blocks_contiguous_append_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: PathCloneAppendRange<'_>,
    destination_path: &str,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    if source.block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "contiguous clone append requires at least one source block",
        ));
    }

    recover_journal_and_checkpoint(device, *superblock)?;
    let source_inode = resolve_path_following_symlinks(device, superblock, source.path)?;
    let destination_inode = resolve_path_following_symlinks(device, superblock, destination_path)?;

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

    append_file_blocks_contiguous_journaled(device, superblock, destination_inode, &blocks)
}
