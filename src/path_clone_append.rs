use std::io;

use crate::block::BlockDevice;
use crate::file_clone_append::clone_file_blocks_append_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathCloneAppendRange<'a> {
    pub path: &'a str,
    pub start: usize,
    pub block_count: usize,
}

/// Atomically clones a non-empty logical-block range from one pathname and appends fresh copies to
/// another pathname.
///
/// Any older committed WAL is recovered and checkpointed before either pathname is resolved, so
/// source and destination inode selection never observes a partially replayed namespace. Source and
/// destination then follow intermediate and final symbolic links with the repository-wide bounded
/// expansion rules. The resolved inode IDs are delegated to
/// [`clone_file_blocks_append_journaled`], which snapshots source data before destination mutation
/// and publishes allocator ownership, destination inode growth, and cloned data homes through one
/// WAL transaction.
///
/// Format v5 has no persisted byte length, so this operation remains block-granular and does not
/// define EOF, sparse-hole, extent, reflink, or broader POSIX copy semantics.
///
/// # Errors
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all
/// [`clone_file_blocks_append_journaled`] validation or durable I/O errors, including non-file
/// endpoints, an empty or out-of-range source interval, allocator exhaustion, ownership
/// disagreement, or insufficient journal capacity.
pub fn clone_file_blocks_append_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: PathCloneAppendRange<'_>,
    destination_path: &str,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let source_inode = resolve_path_following_symlinks(device, superblock, source.path)?;
    let destination_inode = resolve_path_following_symlinks(device, superblock, destination_path)?;
    clone_file_blocks_append_journaled(
        device,
        superblock,
        source_inode,
        source.start,
        source.block_count,
        destination_inode,
    )
}
