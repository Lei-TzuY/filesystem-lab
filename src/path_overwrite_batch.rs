use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_overwrite_batch::write_file_blocks_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically overwrites multiple existing logical blocks of the regular file named by an absolute
/// pathname.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution so target inode
/// selection is derived from recovered namespace state. Path resolution then follows intermediate
/// and final symbolic links with the repository-wide bounded expansion rules. The resolved inode is
/// delegated directly to [`write_file_blocks_journaled`], so validation, allocator-ownership checks,
/// WAL publication, recovery, and checkpoint semantics stay centralized in the existing inode-ID
/// primitive.
///
/// Format v5 does not persist byte length. This operation is therefore full-block and existing-range
/// only: it does not extend files, allocate blocks, create sparse holes, or define extent semantics.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all
/// [`write_file_blocks_journaled`] validation or durable I/O errors, including non-file targets,
/// empty batches, duplicate or out-of-range logical indices, allocator ownership disagreement, and
/// insufficient journal capacity.
pub fn write_file_blocks_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    writes: &[(usize, [u8; BLOCK_SIZE])],
) -> io::Result<RecoveryReport> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    write_file_blocks_journaled(device, superblock, inode_id, writes)
}
