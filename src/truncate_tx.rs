use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::{load_allocator, store_allocator};
use crate::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

/// Atomically shrinks one durable regular file to an exact byte EOF.
///
/// The target may stay inside the current final block or remove any trailing block suffix. Released
/// blocks, the updated inode EOF/block vector, and zeroing of bytes after a partial final EOF are
/// published through one bounded WAL transaction. Zeroing the unused tail prevents bytes discarded
/// by truncate from becoming visible if a later phase adds file extension semantics.
///
/// Growth and sparse holes remain unsupported in this milestone.
///
/// # Errors
///
/// Returns `InvalidInput` for a missing/non-file inode or a target larger than the current EOF.
/// Returns `InvalidData` for allocator ownership disagreement. Journal-capacity, encoding, recovery,
/// checkpoint, and block-device I/O failures are propagated.
pub fn truncate_file_to_bytes_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    target_bytes: u64,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    let Some(plan) = prepare_byte_truncate_plan(device, superblock, inode_id, target_bytes)? else {
        return Ok((Vec::new(), RecoveryReport::default()));
    };

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &plan.allocator)?;
    store_inode_table(&mut capture, superblock, &plan.inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "byte-truncate image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "byte-truncate image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("byte-truncate image rendered outside allocation and inode regions")?;
    if let Some(write) = plan.final_data_write {
        changed.push(write);
    }

    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for (block, image) in changed.iter().copied() {
        log.write(txid, block, image)?;
    }
    log.commit(txid)?;
    store_journal_image(device, *superblock, log.entries())?;

    let report = recover_journal_and_checkpoint(device, *superblock)?;
    if report.committed_transactions != 1 || report.home_writes != changed.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "byte-truncate recovery report is inconsistent",
        ));
    }
    Ok((plan.released, report))
}

struct ByteTruncatePlan {
    allocator: BlockAllocator,
    inodes: Vec<PersistedInode>,
    released: Vec<u64>,
    final_data_write: Option<(u64, [u8; BLOCK_SIZE])>,
}

fn prepare_byte_truncate_plan(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    target_bytes: u64,
) -> io::Result<Option<ByteTruncatePlan>> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let target = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "byte-truncate target inode is missing",
            )
        })?;
    if target.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "byte-truncate target must be a regular file",
        ));
    }

    let current_bytes = target
        .canonical_byte_len()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if target_bytes > current_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "byte-truncate target exceeds current EOF",
        ));
    }
    if target_bytes == current_bytes {
        return Ok(None);
    }

    let target_blocks = byte_len_to_block_count(target_bytes)?;
    if target_blocks > target.blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "persisted EOF requires more blocks than inode references",
        ));
    }
    validate_released_ownership(&allocator, &target.blocks[target_blocks..])?;

    let final_data_write =
        prepare_partial_tail_zero(device, &allocator, target, target_blocks, target_bytes)?;

    let current_blocks = target.blocks.len();
    let released = target.replace_block_range(target_blocks..current_blocks, &[])?;
    target.set_file_byte_len(target_bytes)?;
    for block in &released {
        allocator
            .free(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }

    Ok(Some(ByteTruncatePlan {
        allocator,
        inodes,
        released,
        final_data_write,
    }))
}

fn byte_len_to_block_count(target_bytes: u64) -> io::Result<usize> {
    let blocks = if target_bytes == 0 {
        0
    } else {
        target_bytes.div_ceil(BLOCK_SIZE_U64)
    };
    usize::try_from(blocks).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "byte-truncate block count exceeds usize",
        )
    })
}

fn validate_released_ownership(
    allocator: &BlockAllocator,
    released: &[u64],
) -> io::Result<()> {
    for block in released {
        let owned = allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        if !owned {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "byte-truncate released block is not allocator-owned",
            ));
        }
    }
    Ok(())
}

fn prepare_partial_tail_zero(
    device: &mut impl BlockDevice,
    allocator: &BlockAllocator,
    target: &PersistedInode,
    target_blocks: usize,
    target_bytes: u64,
) -> io::Result<Option<(u64, [u8; BLOCK_SIZE])>> {
    let partial_tail = usize::try_from(target_bytes % BLOCK_SIZE_U64).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "byte-truncate tail offset exceeds usize",
        )
    })?;
    if target_blocks == 0 || partial_tail == 0 {
        return Ok(None);
    }

    let block = target.blocks[target_blocks - 1];
    let owned = allocator
        .is_owned(block)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !owned {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "byte-truncate final block is not allocator-owned",
        ));
    }

    let mut image = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut image)?;
    let original = image;
    image[partial_tail..].fill(0);
    Ok((image != original).then_some((block, image)))
}

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

    let current_blocks = target.blocks.len();
    let released = target.replace_block_range(0..current_blocks, &[])?;
    for block in released {
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
/// This compatibility surface changes only the logical block suffix. When starting from a partial
/// EOF it preserves the unused tail offset through the shared inode mutation boundary. Arbitrary
/// exact-byte shrink is provided separately; growth and sparse files remain outside this contract.
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

    let current_blocks = target.blocks.len();
    let released = target.replace_block_range(target_blocks..current_blocks, &[])?;
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
/// This compatibility operation remains block-granular and preserves a partial EOF tail offset when
/// one already exists. Exact byte shrink is provided by [`truncate_file_to_bytes_journaled`].
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

    let last_index = target.blocks.len().checked_sub(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "truncate target already has zero blocks",
        )
    })?;
    let block = target.blocks[last_index];
    target.replace_block_range(last_index..last_index + 1, &[])?;
    allocator
        .free(block)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

    let report =
        store_create_metadata_journaled(device, superblock, &allocator, &inodes, &entries)?;
    Ok((block, report))
}
