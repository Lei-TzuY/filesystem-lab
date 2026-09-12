use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_replace_contiguous::replace_file_blocks_contiguous_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically replaces a non-empty logical-block range at a pathname target with one contiguous run.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. Intermediate
/// and final symbolic links use the repository-wide bounded resolver. The resolved inode is then
/// delegated to [`replace_file_blocks_contiguous_journaled`], which publishes allocator ownership,
/// inode block-vector replacement, released old ownership, and replacement data images in one WAL
/// transaction.
///
/// Format v5 continues to persist an explicit block vector. This API does not create a durable extent
/// record, sparse semantics, byte-level EOF, reflinks, or POSIX byte-range replacement semantics.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all contiguous replacement
/// validation or durable I/O errors, including empty input, invalid logical ranges, a resolved
/// non-file inode, allocator exhaustion or fragmentation, and journal-capacity failure.
pub fn replace_file_blocks_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    start: usize,
    remove_count: usize,
    replacements: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    replace_file_blocks_contiguous_journaled(
        device,
        superblock,
        inode_id,
        start,
        remove_count,
        replacements,
    )
}
