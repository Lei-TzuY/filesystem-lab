use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_insert_batch::insert_file_blocks_journaled;
use crate::format::Superblock;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically inserts one or more complete logical blocks into the regular file named by an
/// absolute pathname.
///
/// Any older durable WAL is recovered and checkpointed before pathname resolution so intermediate
/// or final symbolic links are resolved from the recovered namespace. Path resolution then follows
/// symbolic links with the repository-wide bounded expansion rules, and the resolved inode is
/// delegated directly to [`insert_file_blocks_journaled`], keeping allocation, inode growth, new
/// data-block publication, WAL recovery, and checkpoint semantics centralized in the inode-ID-based
/// primitive.
///
/// Format v5 has no persisted byte length, so this operation is deliberately block-granular. It
/// does not claim byte-range insertion, EOF, sparse-hole, `fallocate`, or extent semantics.
///
/// # Errors
///
/// Propagates recovery, pathname lookup, and all [`insert_file_blocks_journaled`] validation or
/// durable I/O errors, including a resolved non-file inode, an empty insertion, an insertion index
/// beyond the current logical block count, insufficient free space, or insufficient journal
/// capacity.
pub fn insert_file_blocks_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    insert_index: usize,
    data_blocks: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    insert_file_blocks_journaled(device, superblock, inode_id, insert_index, data_blocks)
}
