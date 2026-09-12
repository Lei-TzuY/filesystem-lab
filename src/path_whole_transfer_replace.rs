use std::io;

use crate::block::BlockDevice;
use crate::file_whole_transfer_replace::transfer_replace_complete_file_blocks_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically moves the complete persisted contents of one existing regular file over another.
///
/// Older committed WAL is recovered and checkpointed before either pathname is resolved. Both
/// pathnames follow intermediate and final symbolic links using the repository-wide bounded
/// expansion rules. The resolved inode IDs are then delegated to
/// [`transfer_replace_complete_file_blocks_journaled`]. The source inode becomes empty, the
/// destination inode receives the source's previous block vector, and every displaced destination
/// block is released from allocator ownership in the same WAL transaction.
///
/// Namespace state and inode identities are unchanged. Format v5 has no persisted byte EOF, so the
/// complete file is exactly its sequence of full 4 KiB logical blocks. No on-disk format change is
/// introduced and this operation does not claim sparse, extent, reflink/COW, or byte-range semantics.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, bounded pathname lookup errors, and all
/// [`transfer_replace_complete_file_blocks_journaled`] validation or durable I/O errors. In
/// particular, both paths must resolve to distinct regular-file inodes.
pub fn transfer_replace_complete_file_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source_path: &str,
    destination_path: &str,
) -> io::Result<RecoveryReport> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let source_inode = resolve_path_following_symlinks(device, superblock, source_path)?;
    let destination_inode =
        resolve_path_following_symlinks(device, superblock, destination_path)?;
    transfer_replace_complete_file_blocks_journaled(
        device,
        superblock,
        source_inode,
        destination_inode,
    )
}
