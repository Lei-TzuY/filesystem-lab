use std::io;

use crate::block::BlockDevice;
use crate::file_data::write_file_range_journaled;
use crate::file_range_read::read_file_range;
use crate::format::Superblock;
use crate::recovery::RecoveryReport;

/// Identifies a byte-range endpoint inside an existing regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileRangeEndpoint {
    pub inode: u64,
    pub first_block: usize,
    pub offset: usize,
}

/// Copies a non-empty byte range between existing regular-file blocks atomically at the destination.
///
/// The complete source range is read into memory before any destination write is published. This
/// gives same-inode overlapping copies snapshot semantics rather than allowing earlier destination
/// writes to feed later source reads. The destination update is then committed through the existing
/// cross-block WAL write path, so recovery exposes either the old destination range or the complete
/// copied range.
///
/// Format v5 has no persisted byte length. Both source and destination therefore must already have
/// every touched logical block; this operation does not allocate, extend files, infer EOF, or create
/// sparse holes.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty range, invalid offsets, missing/non-file inodes, or source or
/// destination ranges that extend beyond existing logical blocks. Returns `InvalidData` for allocator
/// ownership disagreement. Journal, recovery, checkpoint, and block-device I/O errors from the
/// destination write are propagated.
pub fn copy_file_range_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: FileRangeEndpoint,
    destination: FileRangeEndpoint,
    len: usize,
) -> io::Result<RecoveryReport> {
    let snapshot = read_file_range(
        device,
        superblock,
        source.inode,
        source.first_block,
        source.offset,
        len,
    )?;
    write_file_range_journaled(
        device,
        superblock,
        destination.inode,
        destination.first_block,
        destination.offset,
        &snapshot,
    )
}
