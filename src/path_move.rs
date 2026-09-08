use std::io;

use crate::block::BlockDevice;
use crate::file_transfer::move_file_block_range_journaled;
use crate::format::Superblock;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathFileBlockMove<'a> {
    pub path: &'a str,
    pub source_index: usize,
    pub block_count: usize,
    pub destination_index: usize,
}

/// Atomically reorders one contiguous logical-block range inside a regular file addressed by path.
///
/// The pathname follows intermediate and final symbolic links using the repository-wide bounded
/// pathname expansion rules. The resolved inode ID is delegated to
/// [`move_file_block_range_journaled`], which changes only that inode's logical block-reference
/// ordering and publishes the complete inode-table mutation through the existing WAL.
///
/// `destination_index` is interpreted against the logical-block vector after the source range has
/// been removed, matching the inode-ID primitive. Format v5 has no persisted byte length, so this
/// operation is deliberately block-granular and does not define byte-range move, EOF, sparse-hole,
/// extent, reflink, or POSIX semantics.
///
/// # Errors
/// Propagates pathname lookup errors and all [`move_file_block_range_journaled`] validation or
/// durable I/O errors, including a non-file target, empty or out-of-range source interval, invalid
/// post-removal destination boundary, a no-op move, ownership disagreement, or journal errors.
pub fn move_file_block_range_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    operation: PathFileBlockMove<'_>,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    let inode_id = resolve_path_following_symlinks(device, superblock, operation.path)?;
    move_file_block_range_journaled(
        device,
        superblock,
        inode_id,
        operation.source_index,
        operation.block_count,
        operation.destination_index,
    )
}
