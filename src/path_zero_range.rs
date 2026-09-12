use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_zero_range::zero_file_range_journaled;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::path_metadata::metadata_at_path;
use crate::recovery::RecoveryReport;

/// Atomically zeroes a byte range inside existing blocks of the regular file named by an absolute
/// pathname.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution, so inode
/// selection never observes a partially replayed namespace. Path resolution then follows
/// intermediate and final symbolic links with the repository-wide bounded expansion rules. The
/// resolved inode is delegated directly to [`zero_file_range_journaled`], so existing-range
/// validation, WAL publication, crash recovery, and checkpoint semantics remain centralized in the
/// inode-ID-based primitive.
///
/// Format v5 has no persisted byte length. This operation therefore cannot extend a file, infer EOF,
/// allocate blocks, create sparse holes, or provide POSIX `fallocate` semantics.
///
/// # Errors
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all
/// [`zero_file_range_journaled`] validation or durable I/O errors, including a resolved non-file
/// inode, an empty range, an invalid offset, or a range that extends beyond existing logical blocks.
pub fn zero_file_range_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    first_block: usize,
    offset: usize,
    len: usize,
) -> io::Result<RecoveryReport> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    zero_file_range_journaled(device, superblock, inode_id, first_block, offset, len)
}

/// Atomically zeroes every persisted logical block of the regular file named by an absolute path.
///
/// The pathname follows intermediate and final symbolic links. The operation preserves the inode's
/// logical-block vector and allocator ownership exactly; only the complete data-block images change.
/// A zero-block regular file is a validated no-op after recovery/checkpoint and returns an empty
/// recovery report. Non-empty files delegate to [`zero_file_range_at_path_journaled`] for one WAL
/// publication spanning the complete persisted block capacity.
///
/// Format v5 has no byte-level EOF, so "whole file" here means exactly `logical_blocks * 4096`
/// persisted bytes. This does not add sparse-hole, partial-final-block, extent, or byte-length
/// semantics and does not change the on-disk format.
///
/// # Errors
/// Propagates recovery/checkpoint and pathname metadata errors. Returns `InvalidInput` when the path
/// does not resolve to a regular file or when its persisted block count cannot be represented as a
/// byte length. Durable I/O and zero-range publication failures are propagated.
pub fn zero_file_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<RecoveryReport> {
    let metadata = metadata_at_path(device, superblock, path)?;
    if metadata.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pathname whole-file zero target must be a regular file",
        ));
    }
    if metadata.logical_blocks == 0 {
        return Ok(RecoveryReport::default());
    }
    let len = metadata.logical_blocks.checked_mul(BLOCK_SIZE).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "pathname whole-file zero block count overflows byte length",
        )
    })?;
    zero_file_range_at_path_journaled(device, superblock, path, 0, 0, len)
}
