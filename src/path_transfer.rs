use std::io;

use crate::block::BlockDevice;
use crate::file_transfer::transfer_file_block_range_journaled;
use crate::format::Superblock;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PathFileBlockTransfer<'a> {
    pub path: &'a str,
    pub index: usize,
}

/// Atomically transfers a contiguous logical-block range between regular files addressed by paths.
///
/// Both endpoints follow intermediate and final symbolic links using the repository-wide bounded
/// pathname expansion rules. The resolved inode IDs are delegated to
/// [`transfer_file_block_range_journaled`], which removes the selected physical block references
/// from the source and inserts them at the destination without copying data or changing allocator
/// ownership. The complete inode-table mutation is published through the existing WAL.
///
/// Format v5 has no persisted byte length. This operation is deliberately block-granular and does
/// not define byte-range move, EOF, sparse-hole, extent, reflink, or POSIX semantics.
///
/// # Errors
/// Propagates pathname lookup errors and all [`transfer_file_block_range_journaled`] validation or
/// durable I/O errors, including identical/non-file endpoints, an empty or out-of-range source
/// interval, an invalid destination boundary, ownership disagreement, or journal errors.
pub fn transfer_file_block_range_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: PathFileBlockTransfer<'_>,
    block_count: usize,
    destination: PathFileBlockTransfer<'_>,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    let source_inode = resolve_path_following_symlinks(device, superblock, source.path)?;
    let destination_inode = resolve_path_following_symlinks(device, superblock, destination.path)?;
    transfer_file_block_range_journaled(
        device,
        superblock,
        source_inode,
        source.index,
        block_count,
        destination_inode,
        destination.index,
    )
}
