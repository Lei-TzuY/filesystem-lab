use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_data::read_file_block;
use crate::format::Superblock;

/// Reads a non-empty byte range across existing logical blocks of a durable regular file.
///
/// `start_offset` is relative to `first_block_index`. The range may span multiple logical blocks,
/// but every touched block must already be referenced by the inode and allocator-owned. Format v5
/// does not persist a file byte length, so this API intentionally exposes only bytes inside complete
/// logical blocks that already exist; it does not infer EOF, sparse holes, or extension semantics.
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
