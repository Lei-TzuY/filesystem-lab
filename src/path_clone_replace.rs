use std::io;

use crate::block::BlockDevice;
use crate::file_clone_replace::clone_file_blocks_replace_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathCloneReplaceRange<'a> {
    pub path: &'a str,
    pub start: usize,
    pub block_count: usize,
}

/// Atomically replaces an existing destination logical-block range with freshly allocated copies
/// of a non-empty source range selected through pathname resolution.
///
/// Any older committed WAL is recovered and checkpointed before either pathname is resolved, so
/// source and destination inode selection never observes a partially replayed namespace. Source and
/// destination then follow intermediate and final symbolic links with the repository-wide bounded
/// expansion rules. The resolved inode IDs are delegated to
/// [`clone_file_blocks_replace_journaled`], which snapshots source data before destination mutation
/// and publishes allocator ownership, destination inode references, and cloned data homes through
/// one WAL transaction.
///
/// Format v5 has no persisted byte length, so this operation remains block-granular and does not
/// define EOF, partial-block replacement, sparse-hole, extent, reflink, or broader POSIX semantics.
///
/// # Errors
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all
/// [`clone_file_blocks_replace_journaled`] validation or durable I/O errors, including non-file
/// endpoints, an empty or out-of-range interval, identical resolved endpoints, allocator
/// exhaustion, ownership disagreement, or insufficient journal capacity.
pub fn clone_file_blocks_replace_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: PathCloneReplaceRange<'_>,
    destination_path: &str,
    destination_start: usize,
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let source_inode = resolve_path_following_symlinks(device, superblock, source.path)?;
    let destination_inode = resolve_path_following_symlinks(device, superblock, destination_path)?;
    clone_file_blocks_replace_journaled(
        device,
        superblock,
        source_inode,
        source.start,
        source.block_count,
        destination_inode,
        destination_start,
    )
}
