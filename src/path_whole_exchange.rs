use std::io;

use crate::block::BlockDevice;
use crate::file_whole_exchange::exchange_complete_file_blocks_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically exchanges the complete persisted logical-block vectors of two regular files selected
/// by pathname.
///
/// Older committed WAL is recovered and checkpointed before either pathname is resolved. Both
/// endpoints follow intermediate and final symbolic links using the repository-wide bounded
/// expansion rules. The resolved inode IDs are then delegated to
/// [`exchange_complete_file_blocks_journaled`], which swaps only inode block references in one WAL
/// transaction. Physical block data and allocator ownership are unchanged.
///
/// Either file may be empty; exchanging one empty file with one non-empty file therefore transfers
/// the complete persisted contents in one atomic inode-table update. Exchanging two empty files is a
/// validated no-op. Format v5 has no persisted byte EOF, so the complete file is exactly its sequence
/// of full 4 KiB logical blocks. No on-disk format changes are introduced.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, bounded pathname lookup errors, and all
/// [`exchange_complete_file_blocks_journaled`] validation or durable I/O errors. In particular,
/// both paths must resolve to distinct regular-file inodes.
pub fn exchange_complete_files_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    left_path: &str,
    right_path: &str,
) -> io::Result<RecoveryReport> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let left_inode = resolve_path_following_symlinks(device, superblock, left_path)?;
    let right_inode = resolve_path_following_symlinks(device, superblock, right_path)?;
    exchange_complete_file_blocks_journaled(device, superblock, left_inode, right_inode)
}
