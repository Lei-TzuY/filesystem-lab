use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::{load_allocator, store_allocator};
use crate::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

/// Appends multiple complete logical blocks to an existing regular file atomically.
///
/// Allocation ownership, the inode block-list growth, and every new data-block image are published
/// through one WAL transaction. No home location changes before the transaction is durable. After
/// replay makes the new state durable, the fixed journal reservation is checkpointed before success
/// is returned.
///
/// Format v6 preserves exact EOF. Appending whole blocks shifts EOF by exactly the appended block
/// capacity while preserving any existing unused tail offset in the final block. This API remains
/// block-granular; exact-byte zero growth is provided by [`grow_file_to_bytes_journaled`].
///
/// # Errors
///
/// Returns `InvalidInput` for an empty append, a missing/non-file inode, or insufficient free data
/// blocks. Journal-capacity, encoding, I/O, recovery, checkpoint, and flush failures are propagated.
/// A failure may occur after commit is durable; callers must recover and checkpoint before
/// interpreting allocator, inode, or data home state.
pub fn append_file_blocks_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    data_blocks: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    if data_blocks.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multi-block append requires at least one data block",
        ));
    }

    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "file-data target inode is missing",
            )
        })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file-data target must be a regular file",
        ));
    }

    let mut blocks = Vec::with_capacity(data_blocks.len());
    for _ in data_blocks {
        let block = allocator
            .allocate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        blocks.push(block);
    }
    let append_at = inode.blocks.len();
    inode.replace_block_range(append_at..append_at, &blocks)?;

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &allocator)?;
    store_inode_table(&mut capture, superblock, &inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "multi-block append image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "multi-block append image did not render every inode metadata block",
        &mut changed,
    )?;
    capture
        .ensure_empty("multi-block append image rendered outside allocation and inode regions")?;
    changed.extend(blocks.iter().copied().zip(data_blocks.iter().copied()));

    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for (home_block, image) in changed.iter().copied() {
        log.write(txid, home_block, image)?;
    }
    log.commit(txid)?;
    store_journal_image(device, *superblock, log.entries())?;

    let report = recover_journal_and_checkpoint(device, *superblock)?;
    if report.committed_transactions != 1 || report.home_writes != changed.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "multi-block append recovery report is inconsistent",
        ));
    }

    Ok((blocks, report))
}

/// Atomically grows one regular file to an exact larger byte EOF, zero-filling every newly visible
/// byte without creating sparse holes.
///
/// If the new EOF remains inside the current final block, no new block is allocated; bytes after the
/// old EOF are explicitly zeroed before they become visible. If the target crosses a block boundary,
/// only the required trailing blocks are allocated and each new block is published as all zeroes.
/// Allocator ownership, inode block-map/EOF changes, and required data images share one bounded WAL
/// transaction.
///
/// # Errors
///
/// Returns `InvalidInput` for a missing/non-file inode, a target that does not strictly exceed the
/// current EOF, an unrepresentable target block count, or allocation exhaustion. Returns
/// `InvalidData` when the current partial final block is not allocator-owned or recovery reports an
/// inconsistent committed write count. Encoding, journal-capacity, checkpoint, and device I/O
/// failures are propagated.
pub fn grow_file_to_bytes_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    target_bytes: u64,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    let plan = prepare_byte_grow_plan(device, superblock, inode_id, target_bytes)?;

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &plan.allocator)?;
    store_inode_table(&mut capture, superblock, &plan.inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "byte-grow image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "byte-grow image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("byte-grow image rendered outside allocation and inode regions")?;
    if let Some(write) = plan.existing_tail_write {
        changed.push(write);
    }
    changed.extend(
        plan.allocated
            .iter()
            .copied()
            .map(|block| (block, [0_u8; BLOCK_SIZE])),
    );

    let report = publish_byte_grow(device, *superblock, &changed)?;
    Ok((plan.allocated, report))
}

struct ByteGrowPlan {
    allocator: BlockAllocator,
    inodes: Vec<PersistedInode>,
    allocated: Vec<u64>,
    existing_tail_write: Option<(u64, [u8; BLOCK_SIZE])>,
}

fn prepare_byte_grow_plan(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    target_bytes: u64,
) -> io::Result<ByteGrowPlan> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "byte-grow target inode is missing",
            )
        })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "byte-grow target must be a regular file",
        ));
    }

    let current_bytes = inode
        .canonical_byte_len()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if target_bytes <= current_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "byte-grow target must exceed current EOF",
        ));
    }

    let target_blocks = byte_len_to_block_count(target_bytes)?;
    let current_blocks = inode.blocks.len();
    if target_blocks < current_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "byte-grow target requires fewer blocks than current inode mapping",
        ));
    }

    let existing_tail_write = prepare_existing_tail_zero(device, &allocator, inode, current_bytes)?;
    let additional = target_blocks - current_blocks;
    let mut allocated = Vec::with_capacity(additional);
    for _ in 0..additional {
        let block = allocator
            .allocate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        allocated.push(block);
    }
    if !allocated.is_empty() {
        inode.replace_block_range(current_blocks..current_blocks, &allocated)?;
    }
    inode.set_file_byte_len(target_bytes)?;

    Ok(ByteGrowPlan {
        allocator,
        inodes,
        allocated,
        existing_tail_write,
    })
}

fn byte_len_to_block_count(target_bytes: u64) -> io::Result<usize> {
    let blocks = target_bytes.div_ceil(BLOCK_SIZE_U64);
    usize::try_from(blocks).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "byte-grow target block count exceeds usize",
        )
    })
}

fn prepare_existing_tail_zero(
    device: &mut impl BlockDevice,
    allocator: &BlockAllocator,
    inode: &PersistedInode,
    current_bytes: u64,
) -> io::Result<Option<(u64, [u8; BLOCK_SIZE])>> {
    let tail_offset = usize::try_from(current_bytes % BLOCK_SIZE_U64).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "byte-grow tail offset exceeds usize",
        )
    })?;
    if inode.blocks.is_empty() || tail_offset == 0 {
        return Ok(None);
    }

    let block = *inode.blocks.last().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "byte-grow final block missing")
    })?;
    let owned = allocator
        .is_owned(block)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !owned {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "byte-grow final block is not allocator-owned",
        ));
    }

    let mut image = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut image)?;
    let original = image;
    image[tail_offset..].fill(0);
    Ok((image != original).then_some((block, image)))
}

fn publish_byte_grow(
    device: &mut impl BlockDevice,
    superblock: Superblock,
    changed: &[(u64, [u8; BLOCK_SIZE])],
) -> io::Result<RecoveryReport> {
    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for (home_block, image) in changed.iter().copied() {
        log.write(txid, home_block, image)?;
    }
    log.commit(txid)?;
    store_journal_image(device, superblock, log.entries())?;

    let report = recover_journal_and_checkpoint(device, superblock)?;
    if report.committed_transactions != 1 || report.home_writes != changed.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "byte-grow recovery report is inconsistent",
        ));
    }
    Ok(report)
}
