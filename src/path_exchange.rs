use std::io;

use crate::block::BlockDevice;
use crate::file_exchange::{
    exchange_file_block_ranges_journaled, exchange_same_file_block_ranges_journaled,
    exchange_variable_file_block_ranges_journaled, FileBlockExchangeRange,
};
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathFileBlockRange<'a> {
    pub path: &'a str,
    pub start: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathVariableFileBlockRange<'a> {
    pub path: &'a str,
    pub start: usize,
    pub block_count: usize,
}

/// Atomically exchanges equal-length logical-block ranges between regular files addressed by paths.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution so neither
/// endpoint is selected from a partially replayed namespace. Both endpoints then follow intermediate
/// and final symbolic links using the repository-wide bounded pathname expansion rules. The resolved
/// inode IDs are delegated to [`exchange_file_block_ranges_journaled`], so physical blocks are not
/// copied, allocated, or freed; only the two inode block-reference sequences are published through
/// the existing WAL.
///
/// Format v5 has no persisted byte length. This operation is block-granular and requires distinct
/// resolved regular-file inodes and a non-empty range that fits inside both files.
///
/// # Errors
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all
/// [`exchange_file_block_ranges_journaled`] validation or durable I/O errors, including
/// identical/non-file endpoints, a zero block count, out-of-range intervals, duplicate physical
/// references, allocator ownership disagreement, or journal errors.
pub fn exchange_file_block_ranges_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    left: PathFileBlockRange<'_>,
    right: PathFileBlockRange<'_>,
    block_count: usize,
) -> io::Result<RecoveryReport> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let left_inode = resolve_path_following_symlinks(device, superblock, left.path)?;
    let right_inode = resolve_path_following_symlinks(device, superblock, right.path)?;
    exchange_file_block_ranges_journaled(
        device,
        superblock,
        left_inode,
        left.start,
        right_inode,
        right.start,
        block_count,
    )
}

/// Atomically exchanges differently sized logical-block ranges between regular files addressed by
/// paths.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution so neither
/// endpoint is selected from a partially replayed namespace. Both endpoints then follow intermediate
/// and final symbolic links with the repository-wide bounded pathname expansion rules. The resolved
/// inode IDs and requested ranges are delegated to [`exchange_variable_file_block_ranges_journaled`].
/// The selected physical block references trade ownership between the two inode block vectors without
/// allocation, freeing, or data copying. Either file may therefore gain or lose logical blocks while
/// allocator accounting and namespace state remain unchanged.
///
/// Format v5 has no persisted byte length, so this operation is deliberately block-granular and
/// does not define EOF, sparse-hole, extent, reflink, or POSIX range-exchange semantics.
///
/// # Errors
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all
/// [`exchange_variable_file_block_ranges_journaled`] validation or durable I/O errors, including
/// identical/non-file endpoints, empty or overflowing ranges, intervals beyond file end, duplicate
/// physical references, allocator ownership disagreement, inode-capacity errors, or journal errors.
pub fn exchange_variable_file_block_ranges_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    left: PathVariableFileBlockRange<'_>,
    right: PathVariableFileBlockRange<'_>,
) -> io::Result<RecoveryReport> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let left_inode = resolve_path_following_symlinks(device, superblock, left.path)?;
    let right_inode = resolve_path_following_symlinks(device, superblock, right.path)?;
    exchange_variable_file_block_ranges_journaled(
        device,
        superblock,
        FileBlockExchangeRange {
            inode: left_inode,
            start: left.start,
            block_count: left.block_count,
        },
        FileBlockExchangeRange {
            inode: right_inode,
            start: right.start,
            block_count: right.block_count,
        },
    )
}

/// Atomically exchanges two disjoint logical-block ranges within one regular file addressed by
/// absolute pathnames.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution so neither
/// operand is selected from a partially replayed namespace. Both pathname operands then follow
/// intermediate and final symbolic links with the repository-wide bounded expansion rules. They must
/// resolve to the same regular-file inode. Range coordinates refer to that inode's original block
/// vector and may have different lengths, but must be non-empty, disjoint, and entirely within the
/// file. The resolved inode and ranges are delegated to [`exchange_same_file_block_ranges_journaled`],
/// so allocator ownership, block contents, namespace state, WAL ordering, recovery, and checkpoint
/// semantics remain centralized in the existing inode-ID primitive.
///
/// Format v5 has no persisted byte length. This operation is deliberately block-granular and does
/// not define EOF, sparse-hole, extent, reflink, or POSIX range-exchange semantics.
///
/// # Errors
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all
/// [`exchange_same_file_block_ranges_journaled`] validation or durable I/O errors, including operands
/// resolving to different/non-file inodes, empty, overlapping, overflowing, or out-of-range
/// intervals, duplicate physical references, allocator ownership disagreement, or journal errors.
pub fn exchange_same_file_block_ranges_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first: PathVariableFileBlockRange<'_>,
    second: PathVariableFileBlockRange<'_>,
) -> io::Result<RecoveryReport> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let first_inode = resolve_path_following_symlinks(device, superblock, first.path)?;
    let second_inode = resolve_path_following_symlinks(device, superblock, second.path)?;
    exchange_same_file_block_ranges_journaled(
        device,
        superblock,
        FileBlockExchangeRange {
            inode: first_inode,
            start: first.start,
            block_count: first.block_count,
        },
        FileBlockExchangeRange {
            inode: second_inode,
            start: second.start,
            block_count: second.block_count,
        },
    )
}
