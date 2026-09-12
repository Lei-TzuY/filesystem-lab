use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_range_read::read_file_range;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_contiguous_create::create_contiguous_file_with_blocks_at_path_journaled;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Snapshots complete logical blocks from one regular-file pathname and atomically creates a new
/// regular file whose independent physical copies occupy one contiguous lowest-address free run.
///
/// Older committed WAL is recovered and checkpointed before source resolution, so the source inode
/// and any symbolic-link chain are selected from recovered durable namespace state. The requested
/// source range is copied fully into memory before destination mutation begins. Destination
/// publication is delegated to [`create_contiguous_file_with_blocks_at_path_journaled`], which
/// reserves one contiguous physical run and publishes allocation ownership, inode mapping,
/// namespace, and copied data images through one WAL transaction.
///
/// The source is never mutated and destination blocks never share allocator ownership with source
/// blocks. Contiguity is an allocation-time guarantee only: format v5 still persists an explicit
/// inode block vector and gains no extent records, reflink/COW, sparse-hole, or byte-length semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for a zero block count, range-size arithmetic overflow, malformed paths, a
/// source that is not a regular file, an out-of-range source span, destination collision, allocator
/// exhaustion/fragmentation, or insufficient journal capacity. Metadata corruption and durable I/O
/// errors are propagated.
pub fn clone_file_blocks_contiguous_to_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    source_first_block: usize,
    block_count: usize,
    destination: &str,
) -> io::Result<(u64, RecoveryReport)> {
    if block_count == 0 {
        return Err(invalid_input(
            "contiguous pathname clone-create requires at least one logical block",
        ));
    }
    let byte_len = block_count.checked_mul(BLOCK_SIZE).ok_or_else(|| {
        invalid_input("contiguous pathname clone-create block count overflows byte length")
    })?;

    recover_journal_and_checkpoint(device, *superblock)?;
    let source_inode = resolve_path_following_symlinks(device, superblock, source)?;
    let snapshot = read_file_range(
        device,
        superblock,
        source_inode,
        source_first_block,
        0,
        byte_len,
    )?;

    let mut blocks = Vec::with_capacity(block_count);
    for chunk in snapshot.chunks_exact(BLOCK_SIZE) {
        let mut image = [0_u8; BLOCK_SIZE];
        image.copy_from_slice(chunk);
        blocks.push(image);
    }
    debug_assert_eq!(blocks.len(), block_count);

    create_contiguous_file_with_blocks_at_path_journaled(device, superblock, destination, &blocks)
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
