use std::io;

use crate::block::BlockDevice;
use crate::file_remove::remove_file_block_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically removes one complete logical block from the regular file named by an absolute
/// pathname.
///
/// Any older durable WAL is recovered and checkpointed before pathname resolution so intermediate
/// or final symbolic links are resolved from the recovered namespace. Path resolution then follows
/// symbolic links with the repository-wide bounded expansion rules, and the resolved inode is
/// delegated directly to [`remove_file_block_journaled`], keeping allocator release, inode shrink,
/// new WAL publication, recovery, and checkpoint semantics centralized in the inode-ID-based
/// primitive.
///
/// Format v5 has no persisted byte length, so this operation is deliberately block-granular. It
/// does not claim byte-range collapse, EOF, sparse-hole, `fallocate`, or extent semantics.
///
/// # Errors
///
/// Propagates recovery, pathname lookup, and all [`remove_file_block_journaled`] validation or
/// durable I/O errors, including a resolved non-file inode, a logical index outside the current
/// block vector, allocator ownership disagreement, insufficient journal capacity, or device
/// failures.
pub fn remove_file_block_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    remove_index: usize,
) -> io::Result<(u64, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    remove_file_block_journaled(device, superblock, inode_id, remove_index)
}
