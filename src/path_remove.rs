use std::io;

use crate::block::BlockDevice;
use crate::file_remove::remove_file_block_journaled;
use crate::format::Superblock;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically removes one complete logical block from the regular file named by an absolute
/// pathname.
///
/// Path resolution follows intermediate and final symbolic links with the repository-wide bounded
/// expansion rules. The resolved inode is delegated directly to [`remove_file_block_journaled`],
/// keeping allocator release, inode shrink, WAL recovery, and checkpoint semantics centralized in
/// the inode-ID-based primitive.
///
/// Format v5 has no persisted byte length, so this operation is deliberately block-granular. It
/// does not claim byte-range collapse, EOF, sparse-hole, `fallocate`, or extent semantics.
///
/// # Errors
///
/// Propagates pathname lookup errors and all [`remove_file_block_journaled`] validation or durable
/// I/O errors, including a resolved non-file inode, a logical index outside the current block
/// vector, allocator ownership disagreement, insufficient journal capacity, or device failures.
pub fn remove_file_block_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    remove_index: usize,
) -> io::Result<(u64, RecoveryReport)> {
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    remove_file_block_journaled(device, superblock, inode_id, remove_index)
}
