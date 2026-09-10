use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_range_read::read_file_range;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_create::{
    create_empty_file_at_path_journaled, create_file_with_blocks_at_path_journaled,
};
use crate::path_lookup::resolve_path_following_symlinks;
use crate::path_metadata::metadata_at_path;
use crate::recovery::RecoveryReport;

/// Snapshots complete logical blocks from one regular-file pathname and atomically creates a new
/// regular file containing independent physical copies at another pathname.
///
/// Older committed WAL is recovered and checkpointed before source resolution so the source inode
/// and any symbolic-link chain are selected from recovered durable namespace state. The requested
/// source range is then read completely into memory before destination mutation begins. Destination
/// publication is delegated to [`create_file_with_blocks_at_path_journaled`], which resolves its
/// parent from recovered state, allocates distinct physical blocks, and publishes allocation, inode,
/// namespace, and copied data images under one WAL commit.
///
/// The source inode, block references, data, and namespace are never mutated. This is a physical
/// clone, not a reflink: destination blocks have independent allocator ownership. Format v5 has no
/// persisted byte length, so this operation only clones a non-empty range of complete 4 KiB logical
/// blocks and does not define EOF, sparse-hole, shared-extent, or copy-on-write semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for a zero block count, range-size arithmetic overflow, malformed or
/// invalid source/destination paths, a source that is not a regular file, a source range outside the
/// existing block vector, destination collision, allocator exhaustion, or insufficient journal
/// capacity. Metadata corruption and durable device errors are propagated.
pub fn clone_file_blocks_to_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    source_first_block: usize,
    block_count: usize,
    destination: &str,
) -> io::Result<(u64, RecoveryReport)> {
    if block_count == 0 {
        return Err(invalid_input(
            "pathname clone-to-new-file requires at least one logical block",
        ));
    }
    let byte_len = block_count.checked_mul(BLOCK_SIZE).ok_or_else(|| {
        invalid_input("pathname clone-to-new-file block count overflows byte length")
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

    create_file_with_blocks_at_path_journaled(device, superblock, destination, &blocks)
}

/// Clones an entire regular file to a fresh destination pathname.
///
/// Any older committed WAL is recovered and checkpointed before source lookup. The source pathname
/// follows the existing bounded symbolic-link rules and must resolve to a regular file. Because
/// format v5 represents file size only as a vector of complete 4 KiB logical blocks, the whole-file
/// boundary is exactly that vector length. Non-empty files delegate to
/// [`clone_file_blocks_to_path_journaled`] for a complete physical copy; zero-block files delegate to
/// [`create_empty_file_at_path_journaled`] so empty regular files are clonable too.
///
/// The destination receives a fresh inode. For a non-empty source every destination logical block
/// is backed by a newly allocated physical block, so source and destination never share ownership.
/// The source inode, namespace, mappings, and data remain unchanged. This is not reflink/COW and it
/// does not introduce byte-length, partial-block EOF, or sparse-hole semantics.
///
/// # Errors
///
/// Propagates recovery/checkpoint and pathname lookup errors. Returns `InvalidInput` when the source
/// does not resolve to a regular file. Destination collision, allocator exhaustion, journal
/// capacity, metadata corruption, and durable I/O errors are propagated from the bounded create
/// path.
pub fn clone_file_to_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<(u64, RecoveryReport)> {
    let metadata = metadata_at_path(device, superblock, source)?;
    if metadata.kind != InodeKind::File {
        return Err(invalid_input(
            "pathname whole-file clone source must be a regular file",
        ));
    }

    if metadata.logical_blocks == 0 {
        create_empty_file_at_path_journaled(device, superblock, destination)
    } else {
        clone_file_blocks_to_path_journaled(
            device,
            superblock,
            source,
            0,
            metadata.logical_blocks,
            destination,
        )
    }
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
