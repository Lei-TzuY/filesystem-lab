use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_insert_contiguous::insert_file_blocks_contiguous_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically inserts complete logical blocks in one contiguous physical run at a pathname target.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. Intermediate
/// and final symbolic links use the repository-wide bounded resolver. The resolved inode is then
/// delegated to [`insert_file_blocks_contiguous_journaled`], which publishes allocator ownership,
/// inode block-vector insertion, and new data images in one WAL transaction.
///
/// Format v5 continues to persist the inode's explicit block vector. This API does not create a
/// durable extent record, sparse semantics, byte-level EOF, or POSIX byte-range insertion semantics.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all contiguous insertion
/// validation or durable I/O errors, including empty input, an insertion index beyond the resolved
/// file's logical block count, a resolved non-file inode, allocator exhaustion or fragmentation, and
/// journal-capacity failure.
pub fn insert_file_blocks_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    insert_index: usize,
    data_blocks: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    insert_file_blocks_contiguous_journaled(device, superblock, inode_id, insert_index, data_blocks)
}

/// Atomically inserts zero-filled logical blocks in one contiguous physical run at a pathname target.
///
/// This is a block-granular file-growth operation, not sparse insertion: every inserted logical block
/// receives an independently owned physical block whose durable data image is all zeroes. The fresh
/// blocks use the same deterministic lowest-address first-fit contiguous placement and WAL
/// publication contract as [`insert_file_blocks_contiguous_at_path_journaled`]. Existing logical
/// blocks at and after `insert_index` shift right without changing physical ownership. Final symbolic
/// links are followed through the existing bounded pathname resolver.
///
/// Filesystem format remains v5 with explicit inode block vectors. No byte-level EOF, hole, unwritten
/// extent, reservation, or persistent extent-record semantics are introduced.
///
/// # Errors
///
/// Returns `InvalidInput` when `block_count` is zero. Returns `InvalidInput` if the requested count
/// cannot be represented as an in-memory block vector. Propagates invalid insertion-boundary,
/// allocation, pathname, recovery/checkpoint, contiguous-placement, journal-capacity, and durable
/// I/O errors.
pub fn insert_zeroed_blocks_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    insert_index: usize,
    block_count: u64,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    if block_count == 0 {
        return Err(invalid_input(
            "contiguous zero insert must contain at least one block",
        ));
    }

    let block_count = usize::try_from(block_count).map_err(|_| {
        invalid_input("contiguous zero insert block count exceeds addressable memory")
    })?;
    let mut data_blocks = Vec::new();
    data_blocks.try_reserve_exact(block_count).map_err(|_| {
        invalid_input("contiguous zero insert block count exceeds staging capacity")
    })?;
    data_blocks.resize(block_count, [0_u8; BLOCK_SIZE]);

    insert_file_blocks_contiguous_at_path_journaled(
        device,
        superblock,
        path,
        insert_index,
        &data_blocks,
    )
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
