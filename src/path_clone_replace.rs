use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_clone_replace::clone_file_blocks_replace_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_file_read::read_file_blocks_at_path;
use crate::path_file_write::replace_file_at_path_journaled;
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

/// Atomically replaces an existing regular file's complete persisted logical-block sequence with
/// independent physical copies of another regular file selected by pathname.
///
/// Older committed WAL state is recovered and checkpointed before both endpoints are resolved. The
/// source and destination must resolve to distinct inodes, including when either pathname traverses
/// symbolic links or names a hard-link alias. The source's complete format-v5 logical-block vector is
/// then snapshotted before destination mutation begins. Destination publication is delegated to
/// [`replace_file_at_path_journaled`], so zero/nonzero growth, shrink, and equal-size replacement each
/// use exactly one existing crash-consistent destination transaction.
///
/// Source inode references, source data, and source namespace are never changed. Replacement blocks
/// are newly allocated by the destination transaction rather than shared with the source, so this is
/// a physical clone rather than reflink/COW. Format v5 has no persisted byte EOF; the complete file is
/// therefore exactly its sequence of full 4 KiB logical blocks, and this API does not claim
/// partial-final-block, sparse-hole, extent, or byte-length semantics. No on-disk format changes.
///
/// # Errors
///
/// Propagates recovery/checkpoint, bounded pathname lookup, whole-file source-read, destination
/// replacement, allocator, journal-capacity, and durable I/O failures. Returns `InvalidInput` when
/// source and destination resolve to the same inode or either endpoint is not a regular file.
pub fn clone_file_to_existing_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source_path: &str,
    destination_path: &str,
) -> io::Result<RecoveryReport> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let source_inode = resolve_path_following_symlinks(device, superblock, source_path)?;
    let destination_inode = resolve_path_following_symlinks(device, superblock, destination_path)?;
    if source_inode == destination_inode {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "whole-file clone replacement requires distinct source and destination inodes",
        ));
    }

    let snapshot = read_file_blocks_at_path(device, superblock, source_path)?;
    let mut blocks = Vec::with_capacity(snapshot.len() / BLOCK_SIZE);
    for chunk in snapshot.chunks_exact(BLOCK_SIZE) {
        let mut image = [0_u8; BLOCK_SIZE];
        image.copy_from_slice(chunk);
        blocks.push(image);
    }
    debug_assert_eq!(snapshot.len(), blocks.len() * BLOCK_SIZE);

    replace_file_at_path_journaled(device, superblock, destination_path, &blocks)
}
