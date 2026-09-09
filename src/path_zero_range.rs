use std::io;

use crate::block::BlockDevice;
use crate::file_zero_range::zero_file_range_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
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
