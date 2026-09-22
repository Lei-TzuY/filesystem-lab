use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_data::read_file_block;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;

/// Reads a non-empty byte range across existing logical blocks of a durable regular file.
///
/// `start_offset` is relative to `first_block_index`. The range may span multiple logical blocks,
/// but every touched byte must lie at or before the format-v6 persisted EOF and every touched block
/// must already be referenced by the inode and allocator-owned. Sparse holes and implicit extension
/// remain unsupported.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty range, an offset outside the first logical block, arithmetic
/// overflow, a missing/non-file inode, or a range that reaches past the inode's existing block list.
/// Returns `InvalidData` when any referenced physical block is not allocator-owned. Underlying
/// metadata decode and block-device read errors are propagated.
pub fn read_file_range(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    first_block_index: usize,
    start_offset: usize,
    len: usize,
) -> io::Result<Vec<u8>> {
    if len == 0 || start_offset >= BLOCK_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-data read range must be non-empty and start within a block",
        ));
    }

    let absolute_start = first_block_index
        .checked_mul(BLOCK_SIZE)
        .and_then(|base| base.checked_add(start_offset))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "file-data absolute byte offset overflow",
            )
        })?;
    let absolute_end = absolute_start.checked_add(len).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-data read range length overflow",
        )
    })?;

    let inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file-data target inode is missing"))?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-data target must be a regular file",
        ));
    }
    let byte_len = usize::try_from(inode.byte_len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "file byte length exceeds platform address space",
        )
    })?;
    if absolute_end > byte_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-data read range extends beyond EOF",
        ));
    }

    let last_byte = start_offset.checked_add(len).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-data read range length overflow",
        )
    })?;
    let block_count = last_byte.div_ceil(BLOCK_SIZE);
    let mut output = Vec::with_capacity(len);
    let mut remaining = len;

    for relative_index in 0..block_count {
        let logical_index = first_block_index
            .checked_add(relative_index)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "file-data logical index overflow",
                )
            })?;
        let image = read_file_block(device, superblock, inode_id, logical_index)?;
        let begin = if relative_index == 0 { start_offset } else { 0 };
        let take = (BLOCK_SIZE - begin).min(remaining);
        output.extend_from_slice(&image[begin..begin + take]);
        remaining -= take;
    }

    debug_assert_eq!(remaining, 0);
    Ok(output)
}
