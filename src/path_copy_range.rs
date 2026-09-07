use std::io;

use crate::block::BlockDevice;
use crate::file_copy_range::{copy_file_range_journaled, FileRangeEndpoint};
use crate::format::Superblock;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically copies a byte range between existing regular-file blocks addressed by absolute paths.
///
/// Source and destination path resolution follow intermediate and final symbolic links with the
/// repository-wide bounded expansion rules. The resolved inode IDs are delegated to
/// [`copy_file_range_journaled`], preserving its snapshot semantics for overlapping same-inode
/// copies and its WAL-backed atomic destination publication.
///
/// Format v5 has no persisted byte length. Both ranges must fit entirely inside already referenced
/// logical blocks; this operation does not allocate, extend files, infer EOF, or create sparse holes.
///
/// # Errors
/// Propagates pathname lookup errors and all [`copy_file_range_journaled`] validation or durable I/O
/// errors, including non-file endpoints, empty ranges, invalid offsets, or ranges beyond existing
/// logical blocks.
pub fn copy_file_range_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source_path: &str,
    source_first_block: usize,
    source_offset: usize,
    destination_path: &str,
    destination_first_block: usize,
    destination_offset: usize,
    len: usize,
) -> io::Result<RecoveryReport> {
    let source_inode = resolve_path_following_symlinks(device, superblock, source_path)?;
    let destination_inode = resolve_path_following_symlinks(device, superblock, destination_path)?;
    copy_file_range_journaled(
        device,
        superblock,
        FileRangeEndpoint {
            inode: source_inode,
            first_block: source_first_block,
            offset: source_offset,
        },
        FileRangeEndpoint {
            inode: destination_inode,
            first_block: destination_first_block,
            offset: destination_offset,
        },
        len,
    )
}
