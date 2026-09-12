use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE_U64};
use crate::format::Superblock;
use crate::fsck::check_device;
use crate::journal_checkpoint::recover_journal_and_checkpoint;

/// Recovered format-v5 block-space accounting suitable for statfs-like consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilesystemSpace {
    pub block_size: u64,
    pub total_blocks: u64,
    pub reserved_blocks: u64,
    pub data_blocks: u64,
    pub allocated_data_blocks: u64,
    pub free_data_blocks: u64,
}

/// Returns trustworthy block-space accounting from recovered durable filesystem state.
///
/// Any committed WAL transaction is recovered and checkpointed before accounting is observed. The
/// resulting home metadata is then checked with the repository-wide read-only fsck before space is
/// reported, so an allocation bitmap that disagrees with inode ownership is rejected instead of
/// advertising blocks as free incorrectly.
///
/// The report is intentionally limited to format-v5 logical-block accounting. It does not fabricate
/// byte-level capacity, quota, inode-capacity, sparse-file, or POSIX permission semantics that the
/// current on-disk format does not persist.
///
/// # Errors
///
/// Propagates recovery/checkpoint, device I/O, superblock/metadata decoding, journal validation, and
/// fsck ownership/namespace consistency failures. The supplied superblock must match the durable
/// filesystem image because recovery is performed before the read-only fsck pass.
pub fn filesystem_space(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
) -> io::Result<FilesystemSpace> {
    recover_journal_and_checkpoint(device, *superblock)?;
    let report = check_device(device)?;
    if report.total_blocks != superblock.total_blocks
        || report.reserved_blocks != superblock.reserved_blocks()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "durable superblock geometry changed during filesystem-space query",
        ));
    }

    Ok(FilesystemSpace {
        block_size: BLOCK_SIZE_U64,
        total_blocks: report.total_blocks,
        reserved_blocks: report.reserved_blocks,
        data_blocks: report.data_blocks,
        allocated_data_blocks: report.allocated_blocks,
        free_data_blocks: report.free_blocks,
    })
}
