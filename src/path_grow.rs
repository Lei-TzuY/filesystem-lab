use std::cmp::Ordering;
use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_append_batch::{append_file_blocks_journaled, grow_file_to_bytes_journaled};
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::path_metadata::metadata_at_path;
use crate::recovery::RecoveryReport;
use crate::truncate_tx::{
    truncate_file_to_blocks_journaled, truncate_file_to_bytes_journaled,
};

/// Atomically grows one regular file to a larger format-v6 logical-block count.
///
/// Pathname metadata lookup first recovers and checkpoints any older committed WAL, so both the
/// selected inode and its current block-vector length come from recovered durable state. Growth then
/// appends enough newly allocated, zero-filled 4 KiB blocks to reach `target_blocks` under the
/// existing allocation+inode+data WAL transaction.
///
/// This compatibility surface remains block-granular even though format v6 persists exact EOF.
/// Appending whole blocks shifts EOF by whole-block capacity while preserving any existing final
/// tail slack. Exact-byte zero growth is provided separately. Existing blocks are never released.
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

/// Atomically grows one pathname-addressed regular file to an exact larger byte EOF.
///
/// Older committed WAL is checked/recovered by metadata lookup before inode selection. Growth
/// zero-fills every newly visible byte and allocates only the trailing blocks required by the new
/// non-sparse EOF. If the target remains inside the current final block, no new block is allocated.
///
/// # Errors
///
/// Returns InvalidInput for non-file targets or a target that does not exceed the current EOF.
/// Allocation exhaustion, inode/allocator disagreement, journal-capacity, recovery/checkpoint, and
/// durable I/O failures are propagated.
pub fn grow_file_at_path_to_bytes_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    target_bytes: u64,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    let metadata = metadata_at_path(device, superblock, path)?;
    if metadata.kind != InodeKind::File {
        return Err(invalid_input(
            "pathname byte-growth target must be a regular file",
        ));
    }
    if target_bytes <= metadata.byte_len {
        return Err(invalid_input(
            "pathname byte-growth target must exceed current EOF",
        ));
    }

    grow_file_to_bytes_journaled(
        device,
        superblock,
        metadata.inode_id,
        target_bytes,
    )
}

/// Atomically resizes one pathname-addressed regular file to an exact byte EOF.
///
/// Growth zero-fills the newly visible range without sparse holes. Shrink delegates to the
/// format-v6 exact-byte truncate transaction, which releases only the trailing block suffix and
/// zeroes bytes discarded after a partial final EOF. Equal-size requests are rejected.
///
/// The returned block vector contains newly allocated blocks for growth or released blocks for
/// shrink.
///
/// # Errors
///
/// Returns InvalidInput for non-file targets or equal-size requests. Growth/shrink validation,
/// allocator ownership, bounded-journal capacity, recovery/checkpoint, and durable I/O errors are
/// propagated.
pub fn resize_file_at_path_to_bytes_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    target_bytes: u64,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    let metadata = metadata_at_path(device, superblock, path)?;
    if metadata.kind != InodeKind::File {
        return Err(invalid_input(
            "pathname byte-resize target must be a regular file",
        ));
    }

    match target_bytes.cmp(&metadata.byte_len) {
        Ordering::Greater => grow_file_to_bytes_journaled(
            device,
            superblock,
            metadata.inode_id,
            target_bytes,
        ),
        Ordering::Less => truncate_file_to_bytes_journaled(
            device,
            superblock,
            metadata.inode_id,
            target_bytes,
        ),
        Ordering::Equal => Err(invalid_input(
            "pathname byte-resize target must differ from current EOF",
        )),
    }
}
/// Atomically resizes one pathname-addressed regular file to an exact format-v6 block count.
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
/// no-op. Format v6 still has no persisted byte length, so this API does not define partial-block
/// sparse holes or implicit partial-block allocation semantics.
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
        Ordering::Less => {
            truncate_file_to_blocks_journaled(device, superblock, metadata.inode_id, target_blocks)
        }
        Ordering::Equal => Err(invalid_input(
            "pathname block-resize target must differ from current logical-block count",
        )),
    }
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
