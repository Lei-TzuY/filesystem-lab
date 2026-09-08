use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::file_replace::replace_file_blocks_journaled;
use crate::format::Superblock;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Atomically replaces one non-empty existing logical-block range of the regular file named by an
/// absolute pathname with a non-empty caller-provided block sequence that may have a different length.
///
/// Path resolution follows intermediate and final symbolic links with the repository-wide bounded
/// expansion rules. The resolved inode is delegated directly to [`replace_file_blocks_journaled`],
/// preserving its allocation, inode-resize, data-publication, WAL recovery, and checkpoint semantics.
///
/// Format v5 has no persisted byte length, so this is deliberately block-granular and does not claim
/// byte-range replacement, EOF, sparse-hole, extent, reflink, or POSIX splice semantics.
///
/// # Errors
///
/// Propagates pathname lookup errors and all [`replace_file_blocks_journaled`] validation or durable
/// I/O errors.
pub fn replace_file_blocks_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    start: usize,
    remove_count: usize,
    replacements: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    replace_file_blocks_journaled(
        device,
        superblock,
        inode_id,
        start,
        remove_count,
        replacements,
    )
}
