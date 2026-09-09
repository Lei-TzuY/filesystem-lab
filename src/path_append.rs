use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_append_batch::append_file_blocks_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically appends complete logical blocks to the regular file named by an absolute pathname.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution so lookup never
/// derives the target inode from a partially replayed namespace. Path resolution then follows
/// intermediate and final symbolic links with the repository-wide bounded expansion rules. The
/// resolved inode is delegated directly to [`append_file_blocks_journaled`], so allocator ownership,
/// inode growth, data publication, WAL ordering, recovery, and journal capacity remain centralized
/// in the existing inode-ID-based append primitive.
///
/// Format v5 does not persist byte length. This operation therefore appends only complete 4 KiB
/// logical blocks and does not define partial-block EOF, sparse-hole, or extent semantics.
///
/// # Errors
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all
/// [`append_file_blocks_journaled`] validation or durable I/O errors, including a resolved non-file
/// inode, an empty append, allocator exhaustion, and insufficient journal capacity.
pub fn append_file_blocks_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    data_blocks: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    append_file_blocks_journaled(device, superblock, inode_id, data_blocks)
}
