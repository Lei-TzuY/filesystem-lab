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
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "truncate target inode is missing",
            )
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

/// Atomically truncates a durable regular file to an exact logical block count.
///
/// Every trailing physical block removed from the inode is released from allocator ownership in the
/// same WAL transaction as the updated inode table. The inode identity and namespace are preserved,
/// and successful home replay is checkpointed before return by the shared metadata transaction path.
/// The operation is a no-op when `target_blocks` equals the current block count.
///
/// Format v5 has no byte-length field, so the target is expressed only as a count of complete 4 KiB
/// logical blocks. Growing a file, partial-block truncation, sparse files, and byte-size semantics are
/// outside this contract.
///
/// # Errors
///
/// Returns `InvalidInput` when the target inode is missing, is not a regular file, or `target_blocks`
/// exceeds its current block count. Returns `InvalidData` when allocator ownership disagrees with any
/// trailing inode reference being released. Durable metadata decoding, journal-capacity, journal I/O,
/// recovery, checkpoint, home-write, and flush failures are propagated.
pub fn truncate_file_to_blocks_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    target_blocks: usize,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;

    let target = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "truncate target inode is missing",
            )
        })?;
    if target.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "truncate target must be a regular file",
        ));
    }
    if target_blocks > target.blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "truncate target block count exceeds current file blocks",
        ));
    }
    if target_blocks == target.blocks.len() {
        return Ok((Vec::new(), RecoveryReport::default()));
    }

    let released = target.blocks.split_off(target_blocks);
    for block in &released {
        allocator
            .free(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }

    let report =
        store_create_metadata_journaled(device, superblock, &allocator, &inodes, &entries)?;
    Ok((released, report))
}

/// Atomically removes exactly the final logical block from a durable regular file.
///
/// The inode keeps its identity and namespace entry. The final physical block reference and that
/// block's allocator ownership are advanced together through one WAL transaction, so a completed
/// filesystem state can never expose a freed block that is still referenced by the inode. Successful
/// replay is checkpointed before return by the shared metadata transaction primitive.
///
/// Format v5 does not persist byte length, so this operation is deliberately block-granular. It is
/// the shrink-side counterpart of block append and does not define partial-block truncation or sparse
/// file semantics.
///
/// # Errors
///
/// Returns `InvalidInput` when the target inode is missing, is not a regular file, or already owns no
/// blocks. Returns `InvalidData` if allocator ownership disagrees with the inode's final reference.
/// Durable metadata decoding, journal-capacity, journal-write, recovery, checkpoint, home-write, and
/// flush failures are propagated.
pub fn truncate_file_last_block_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
) -> io::Result<(u64, RecoveryReport)> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;

    let target = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "truncate target inode is missing",
            )
        })?;
    if target.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "truncate target must be a regular file",
        ));
    }

    let block = target.blocks.pop().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "truncate target already has zero blocks",
        )
    })?;
    allocator
        .free(block)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    let report =
        store_create_metadata_journaled(device, superblock, &allocator, &inodes, &entries)?;
    Ok((block, report))
}
