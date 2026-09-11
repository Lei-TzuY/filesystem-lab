use std::cmp::Ordering;
use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_append_batch::append_file_blocks_journaled;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::path_metadata::metadata_at_path;
use crate::recovery::RecoveryReport;
use crate::truncate_tx::truncate_file_to_blocks_journaled;

/// Atomically grows one regular file to a larger format-v5 logical-block count.
///
/// Pathname metadata lookup first recovers and checkpoints any older committed WAL, so both the
/// selected inode and its current block-vector length come from recovered durable state. Growth then
/// appends enough newly allocated, zero-filled 4 KiB blocks to reach `target_blocks` under the
/// existing allocation+inode+data WAL transaction.
///
/// This is deliberately block-granular. Format v5 has no persisted byte length, so the operation
/// does not define partial-block EOF, sparse holes, or POSIX byte-granular `ftruncate` semantics.
/// Existing blocks are never rewritten or released.
///
/// # Errors
///
/// Returns `InvalidInput` when the pathname does not resolve to a regular file, when
/// `target_blocks` is not strictly larger than the current logical-block count, or when the
/// zero-block staging vector cannot be reserved. Allocator exhaustion, journal capacity, metadata
/// corruption, recovery/checkpoint failures, and durable I/O errors are propagated.
pub fn grow_file_at_path_to_blocks_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    target_blocks: usize,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    let metadata = metadata_at_path(device, superblock, path)?;
    if metadata.kind != InodeKind::File {
        return Err(invalid_input(
            "pathname zero-growth target must be a regular file",
        ));
    }
    if target_blocks <= metadata.logical_blocks {
        return Err(invalid_input(
            "pathname zero-growth target must exceed current logical-block count",
        ));
    }

    let additional = target_blocks - metadata.logical_blocks;
    let mut zero_blocks = Vec::new();
    zero_blocks
        .try_reserve_exact(additional)
        .map_err(|_| invalid_input("pathname zero-growth block count exceeds staging capacity"))?;
    zero_blocks.resize(additional, [0_u8; BLOCK_SIZE]);

    append_file_blocks_journaled(device, superblock, metadata.inode_id, &zero_blocks)
}

/// Atomically resizes one pathname-addressed regular file to an exact format-v5 block count.
///
/// This is the bidirectional block-granular size-control surface for format v5. The pathname is
/// resolved only after older committed WAL has been recovered by [`metadata_at_path`]. Growing
/// delegates to [`grow_file_at_path_to_blocks_journaled`], which allocates newly owned zero-filled
/// blocks. Shrinking delegates directly to [`truncate_file_to_blocks_journaled`], which releases
/// the exact trailing block suffix under the allocation+inode WAL transaction.
///
/// The returned block vector describes the physical blocks whose ownership changed: newly allocated
/// blocks for growth, or released trailing blocks for shrink. Equal-size requests are rejected so a
/// successful call always corresponds to one durable size transition rather than an ambiguous
/// no-op. Format v5 still has no persisted byte length, so this API does not define partial-block
/// EOF, sparse holes, or byte-granular POSIX `ftruncate` semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for non-file targets or equal-size requests. Growth and shrink validation,
/// allocator ownership checks, journal-capacity checks, metadata corruption, and durable I/O errors
/// are propagated from their existing transaction primitives.
pub fn resize_file_at_path_to_blocks_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    target_blocks: usize,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    let metadata = metadata_at_path(device, superblock, path)?;
    if metadata.kind != InodeKind::File {
        return Err(invalid_input(
            "pathname block-resize target must be a regular file",
        ));
    }

    match target_blocks.cmp(&metadata.logical_blocks) {
        Ordering::Greater => {
            grow_file_at_path_to_blocks_journaled(device, superblock, path, target_blocks)
        }
        Ordering::Less => truncate_file_to_blocks_journaled(
            device,
            superblock,
            metadata.inode_id,
            target_blocks,
        ),
        Ordering::Equal => Err(invalid_input(
            "pathname block-resize target must differ from current logical-block count",
        )),
    }
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
