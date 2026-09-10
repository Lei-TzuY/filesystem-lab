use std::io;

use crate::block::BlockDevice;
use crate::file_clone_splice::{clone_file_blocks_splice_journaled, FileBlockRange};
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathCloneSpliceRange<'a> {
    pub path: &'a str,
    pub start: usize,
    pub block_count: usize,
}

/// Atomically replaces a destination logical-block range with a differently sized fresh clone range
/// selected through bounded pathname resolution.
///
/// Any older committed journal transaction is recovered and checkpointed before either pathname is
/// resolved, so endpoint selection observes the recovered namespace. Source and destination both
/// follow intermediate and final symbolic links under the repository-wide bounded expansion rules.
/// Resolved inode IDs are delegated to [`clone_file_blocks_splice_journaled`], preserving its single
/// WAL-backed allocator/inode/data publication path and recovery contract.
///
/// Format v5 has no persisted byte length, so this operation remains block-granular and does not
/// define EOF, partial-block splice behavior, sparse holes, extents, reflinks, or broader POSIX
/// semantics.
///
/// # Errors
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all
/// [`clone_file_blocks_splice_journaled`] validation or durable I/O errors, including non-file
/// endpoints, empty or out-of-range intervals, identical resolved endpoints, allocator exhaustion,
/// ownership disagreement, or insufficient journal capacity.
pub fn clone_file_blocks_splice_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: PathCloneSpliceRange<'_>,
    destination: PathCloneSpliceRange<'_>,
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let source_inode = resolve_path_following_symlinks(device, superblock, source.path)?;
    let destination_inode = resolve_path_following_symlinks(device, superblock, destination.path)?;
    clone_file_blocks_splice_journaled(
        device,
        superblock,
        FileBlockRange {
            inode: source_inode,
            start: source.start,
            block_count: source.block_count,
        },
        FileBlockRange {
            inode: destination_inode,
            start: destination.start,
            block_count: destination.block_count,
        },
    )
}
