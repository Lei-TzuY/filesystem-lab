use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::recovery::RecoveryReport;

/// Atomically truncates one durable regular file to zero owned blocks.
///
/// This bounded lifecycle operation preserves the inode and namespace while removing every durable
/// block reference from the target inode and releasing exactly those blocks in the allocator. The
/// allocation and inode home images are committed through one WAL transaction, so recovery cannot
/// make a freed block coexist with a surviving inode reference as a completed filesystem state.
///
/// File byte length is not modeled by format v5, so this primitive deliberately supports only the
/// unambiguous zero-block truncation boundary. Partial-block truncation, sparse files, and data-write
/// ordering remain outside this contract.
///
/// # Errors
///
/// Returns `InvalidInput` when the target inode is missing or is not a regular file. Durable metadata
/// decoding, allocator, journal-capacity, journal-write, recovery, home-write, and flush failures are
/// propagated. A home-write failure may happen after the commit is durable; callers must recover the
/// journal before interpreting home metadata.
pub fn truncate_file_to_zero_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
) -> io::Result<RecoveryReport> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;

    let target = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "truncate target inode is missing")
        })?;
    if target.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "truncate target must be a regular file",
        ));
    }

    if target.blocks.is_empty() {
        return Ok(RecoveryReport::default());
    }

    for block in target.blocks.drain(..) {
        allocator
            .free(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }

    store_create_metadata_journaled(device, superblock, &allocator, &inodes, &entries)
}
