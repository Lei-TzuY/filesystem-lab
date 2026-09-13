use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_replace_contiguous::replace_file_blocks_contiguous_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically replaces a non-empty logical-block range at a pathname target with one contiguous run.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. Intermediate
/// and final symbolic links use the repository-wide bounded resolver. The resolved inode is then
/// delegated to [`replace_file_blocks_contiguous_journaled`], which publishes allocator ownership,
/// inode block-vector replacement, released old ownership, and replacement data images in one WAL
/// transaction.
///
/// Format v5 continues to persist an explicit block vector. This API does not create a durable extent
/// record, sparse semantics, byte-level EOF, reflinks, or POSIX byte-range replacement semantics.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all contiguous replacement
/// validation or durable I/O errors, including empty input, invalid logical ranges, a resolved
/// non-file inode, allocator exhaustion or fragmentation, and journal-capacity failure.
pub fn replace_file_blocks_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    start: usize,
    remove_count: usize,
    replacements: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    replace_file_blocks_contiguous_journaled(
        device,
        superblock,
        inode_id,
        start,
        remove_count,
        replacements,
    )
}

/// Atomically replaces an existing logical-block range with freshly allocated zero-filled blocks.
///
/// The replacement has exactly the same logical length as the removed range, so this operation
/// preserves the file's block count. Every replacement logical block receives a newly allocated,
/// independently owned physical block whose durable image is all zeroes. The fresh blocks are
/// allocated as one deterministic lowest-address contiguous run and are published together with the
/// updated inode mapping and released old ownership through the existing contiguous replacement WAL
/// transaction. Intermediate and final symbolic links are followed by the bounded pathname resolver.
///
/// This is real allocation-backed replacement, not sparse zeroing, hole punching, reservation, or an
/// unwritten extent. Filesystem format remains v5 with explicit inode block vectors.
///
/// # Errors
///
/// Returns `InvalidInput` when `block_count` is zero, when the count cannot be represented as an
/// in-memory replacement vector, or when the requested logical range is invalid. Propagates pathname,
/// recovery/checkpoint, allocator ownership, contiguous-placement, journal-capacity, and durable I/O
/// failures.
pub fn replace_with_zeroed_blocks_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    start: usize,
    block_count: u64,
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    if block_count == 0 {
        return Err(invalid_input(
            "contiguous zero replacement must contain at least one block",
        ));
    }

    let block_count = usize::try_from(block_count).map_err(|_| {
        invalid_input("contiguous zero replacement block count exceeds addressable memory")
    })?;
    let mut replacements = Vec::new();
    replacements.try_reserve_exact(block_count).map_err(|_| {
        invalid_input("contiguous zero replacement block count exceeds staging capacity")
    })?;
    replacements.resize(block_count, [0_u8; BLOCK_SIZE]);

    replace_file_blocks_contiguous_at_path_journaled(
        device,
        superblock,
        path,
        start,
        block_count,
        &replacements,
    )
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
