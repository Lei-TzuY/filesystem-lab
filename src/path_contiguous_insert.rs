use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_insert_contiguous::insert_file_blocks_contiguous_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically inserts complete logical blocks in one contiguous physical run at a pathname target.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. Intermediate
/// and final symbolic links use the repository-wide bounded resolver. The resolved inode is then
/// delegated to [`insert_file_blocks_contiguous_journaled`], which publishes allocator ownership,
/// inode block-vector insertion, and new data images in one WAL transaction.
///
/// Format v5 continues to persist the inode's explicit block vector. This API does not create a
/// durable extent record, sparse semantics, byte-level EOF, or POSIX byte-range insertion semantics.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, pathname lookup errors, and all contiguous insertion
/// validation or durable I/O errors, including empty input, an insertion index beyond the resolved
/// file's logical block count, a resolved non-file inode, allocator exhaustion or fragmentation, and
/// journal-capacity failure.
pub fn insert_file_blocks_contiguous_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    insert_index: usize,
    data_blocks: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    insert_file_blocks_contiguous_journaled(device, superblock, inode_id, insert_index, data_blocks)
}
