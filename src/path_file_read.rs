use std::io;

use crate::block::BlockDevice;
use crate::file_range_read::read_file_range;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint_checked;
use crate::path_lookup::resolve_path_following_symlinks;

/// Reads every byte through the persisted EOF of the regular file named by an absolute pathname.
///
/// Any older committed WAL is recovered and checkpointed before pathname resolution. Resolution
/// follows intermediate and final symbolic links. The resolved inode must be a regular file. A
/// zero-block file returns an empty vector; otherwise the operation delegates the complete block
/// range to the existing range-read primitive so allocator ownership validation remains centralized.
///
/// Format v6 persists exact regular-file EOF. The final logical block is trimmed to that byte
/// length; sparse holes remain unsupported. The operation changes no durable state.
///
/// # Errors
///
/// Propagates recovery/checkpoint failures, bounded pathname lookup, persisted inode-table decode
/// errors, and existing regular-file range-read validation. Returns `InvalidInput` when the resolved
/// inode is not a regular file and `InvalidData` if lookup resolves an inode absent from the table.
pub fn read_file_blocks_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<Vec<u8>> {
    recover_journal_and_checkpoint_checked(device, *superblock)?;
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
    if inode.byte_len == 0 {
        return Ok(Vec::new());
    }
    let len = usize::try_from(inode.byte_len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "whole-file byte length exceeds platform address space",
        )
    })?;
    read_file_range(device, superblock, inode_id, 0, 0, len)
}
