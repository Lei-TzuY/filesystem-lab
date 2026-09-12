use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_append_contiguous::append_file_blocks_contiguous_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically appends complete logical blocks in one contiguous physical run to a pathname target.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. Intermediate
/// and final symbolic links use the repository-wide bounded resolver. The resolved inode is then
/// delegated to [`append_file_blocks_contiguous_journaled`], which publishes allocator ownership,
/// inode growth, and new data images in one WAL transaction.
///
/// Format v5 continues to persist the inode's explicit block vector. This API does not create a
/// durable extent record, sparse semantics, byte-level EOF, or a promise that the appended run is
/// adjacent to the file's previous final block.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all contiguous append
/// validation or durable I/O errors, including empty input, a resolved non-file inode, allocator
/// exhaustion, fragmentation that prevents a sufficiently large run, and journal-capacity failure.
pub fn append_file_blocks_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    data_blocks: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    append_file_blocks_contiguous_journaled(device, superblock, inode_id, data_blocks)
}
