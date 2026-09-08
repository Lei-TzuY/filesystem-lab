use std::io;

use crate::block::BlockDevice;
use crate::file_collapse::collapse_file_block_range_journaled;
use crate::format::Superblock;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically removes a non-empty contiguous logical-block range from the regular file named by an
/// absolute pathname.
///
/// Path resolution follows intermediate and final symbolic links with the repository-wide bounded
/// expansion rules. The resolved inode is delegated directly to
/// [`collapse_file_block_range_journaled`], keeping allocator ownership validation, WAL publication,
/// crash recovery, and checkpoint semantics centralized in the inode-ID-based primitive.
///
/// Format v5 has no persisted byte length, so this operation is deliberately block-granular. It does
/// not claim byte-range collapse, EOF, sparse-hole, `fallocate`, or extent semantics.
///
/// # Errors
///
/// Propagates pathname lookup errors and all [`collapse_file_block_range_journaled`] validation or
/// durable I/O errors, including a resolved non-file inode, an empty range, an out-of-range interval,
/// allocator ownership disagreement, or insufficient journal capacity.
pub fn collapse_file_block_range_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    start_index: usize,
    block_count: usize,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    collapse_file_block_range_journaled(device, superblock, inode_id, start_index, block_count)
}
