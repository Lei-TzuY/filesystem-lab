use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_append_contiguous::append_file_blocks_contiguous_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically appends complete logical blocks in one contiguous physical run to a pathname target.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. Intermediate
/// and final symbolic links use the repository-wide bounded resolver. The resolved inode is then
/// delegated to [`append_file_blocks_contiguous_journaled`], which publishes allocator ownership,
/// inode growth, and new data images in one WAL transaction.
///
/// Format v5 continues to persist the inode's explicit block vector. This API does not create a
/// durable extent record, sparse semantics, byte-level EOF, or a promise that the appended run is
/// adjacent to the file's previous final block.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all contiguous append
/// validation or durable I/O errors, including empty input, a resolved non-file inode, allocator
/// exhaustion, fragmentation that prevents a sufficiently large run, and journal-capacity failure.
pub fn append_file_blocks_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    data_blocks: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    append_file_blocks_contiguous_journaled(device, superblock, inode_id, data_blocks)
}

/// Atomically extends a pathname-resolved regular file with zero-filled blocks in one contiguous run.
///
/// This is a block-granular file-growth operation, not sparse preallocation: every requested logical
/// block receives an independently owned physical block whose durable data image is all zeroes. The
/// fresh blocks use the same deterministic lowest-address first-fit contiguous placement and WAL
/// publication contract as [`append_file_blocks_contiguous_at_path_journaled`]. Final symbolic links
/// are followed through the existing bounded pathname resolver.
///
/// Filesystem format remains v5 with explicit inode block vectors. No byte-level EOF, hole, unwritten
/// extent, reservation, or persistent extent-record semantics are introduced.
///
/// # Errors
///
/// Returns `InvalidInput` when `block_count` is zero. Returns `InvalidInput` if the requested count
/// cannot be represented as an in-memory block vector. Propagates allocation failure, pathname,
/// recovery/checkpoint, contiguous-placement, journal-capacity, and durable I/O errors.
pub fn append_zeroed_blocks_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    block_count: u64,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    if block_count == 0 {
        return Err(invalid_input(
            "contiguous zero append must contain at least one block",
        ));
    }

    let block_count = usize::try_from(block_count).map_err(|_| {
        invalid_input("contiguous zero append block count exceeds addressable memory")
    })?;
    let mut data_blocks = Vec::new();
    data_blocks.try_reserve_exact(block_count).map_err(|_| {
        invalid_input("contiguous zero append block count exceeds staging capacity")
    })?;
    data_blocks.resize(block_count, [0_u8; BLOCK_SIZE]);

    append_file_blocks_contiguous_at_path_journaled(device, superblock, path, &data_blocks)
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
