use std::io;

use crate::block::BlockDevice;
use crate::file_data::write_file_range_journaled;
use crate::format::Superblock;
use crate::recovery::RecoveryReport;

/// Atomically zeroes a non-empty byte range inside existing regular-file blocks.
///
/// The destination range must fit entirely within blocks already referenced by the target inode.
/// Publication is delegated to the existing cross-block WAL write path, so recovery exposes either
/// the complete old range or the complete zeroed range. Allocator ownership, inode block references,
/// and namespace metadata are unchanged.
///
/// Format v5 has no persisted byte length. This operation therefore does not allocate blocks, extend
/// files, infer EOF, create sparse holes, or provide POSIX `fallocate` semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty range, invalid offset, missing/non-file inode, or a range that
/// extends beyond existing logical blocks. Returns `InvalidData` for allocator ownership disagreement.
/// Journal, recovery, checkpoint, and block-device I/O failures are propagated.
pub fn zero_file_range_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    first_block: usize,
    offset: usize,
    len: usize,
) -> io::Result<RecoveryReport> {
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "zero-range requires at least one byte",
        ));
    }
    let zeros = vec![0_u8; len];
    write_file_range_journaled(
        device,
        superblock,
        inode_id,
        first_block,
        offset,
        &zeros,
    )
}
