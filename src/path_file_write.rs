use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_data::write_file_range_journaled;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically overwrites every complete logical block persisted by the regular file named by an
/// absolute pathname.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. Resolution
/// follows intermediate and final symbolic links. The supplied byte slice must exactly match the
/// resolved regular file's persisted logical-block capacity (`logical_blocks * BLOCK_SIZE`). A
/// zero-block file therefore accepts only an empty slice and is a no-op after recovery/checkpoint.
///
/// The mutation delegates non-empty writes to the existing journaled range-write primitive, keeping
/// allocator ownership, WAL publication, recovery, and journal-capacity validation centralized.
/// Format v5 still has no byte EOF, so this API neither grows nor shrinks the file and cannot encode
/// a partial final block or sparse hole. It changes no on-disk format.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, bounded pathname lookup, persisted inode-table decode,
/// and existing range-write validation or durable I/O errors. Returns `InvalidInput` when the
/// resolved inode is not a regular file or when `data.len()` differs from its complete persisted
/// logical-block capacity, and `InvalidData` if lookup resolves an inode absent from the table.
pub fn write_file_blocks_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    data: &[u8],
) -> io::Result<RecoveryReport> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    let inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "resolved inode is missing"))?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resolved pathname is not a regular file",
        ));
    }
    let expected_len = inode.blocks.len().checked_mul(BLOCK_SIZE).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "whole-file logical-block byte length overflow",
        )
    })?;
    if data.len() != expected_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "whole-file write length must match persisted logical-block capacity",
        ));
    }
    if data.is_empty() {
        return Ok(RecoveryReport::default());
    }
    write_file_range_journaled(device, superblock, inode_id, 0, 0, data)
}
