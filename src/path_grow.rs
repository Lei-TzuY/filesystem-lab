use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_append_batch::append_file_blocks_journaled;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::path_metadata::metadata_at_path;
use crate::recovery::RecoveryReport;

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

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
