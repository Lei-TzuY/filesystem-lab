use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::{load_allocator, store_allocator};
use crate::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};
use crate::format::Superblock;
use crate::fsck::check_device;
use crate::inode::InodeKind;
use crate::inode_codec::{PersistedInode, SPARSE_HOLE_BLOCK};
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HolePunchReport {
    pub released_blocks: Vec<u64>,
    pub punched_logical_blocks: Vec<usize>,
    pub zeroed_edge_blocks: Vec<usize>,
    pub transaction: RecoveryReport,
}

struct HolePunchPlan {
    allocator: BlockAllocator,
    inodes: Vec<PersistedInode>,
    released_blocks: Vec<u64>,
    punched_logical_blocks: Vec<usize>,
    zeroed_edge_blocks: Vec<usize>,
    data_writes: Vec<(u64, [u8; BLOCK_SIZE])>,
}

/// Atomically punches one non-empty byte range inside an existing regular-file EOF.
///
/// Every logical block whose complete visible byte interval is covered becomes a sparse-hole
/// sentinel, and any prior physical block backing that slot is released from allocator ownership.
/// Partially covered physical edge blocks remain allocated but have only the selected bytes zeroed.
/// A partial edge that is already sparse is already semantically zero and needs no data write. File
/// EOF and logical block count do not change.
///
/// Allocation, inode mapping, and edge data-block images are committed through one bounded WAL
/// transaction, so recovery cannot expose a freed physical block while the inode still references it.
///
/// # Errors
///
/// Returns InvalidInput for an empty range, a missing/non-file inode, arithmetic overflow, or a range
/// extending beyond EOF. Returns InvalidData for allocator/inode ownership disagreement. Journal-
/// capacity, recovery, checkpoint, fsck, and block-device I/O failures are propagated.
pub fn punch_file_hole_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    offset: u64,
    len: u64,
) -> io::Result<HolePunchReport> {
    let Some(plan) = prepare_hole_punch(device, superblock, inode_id, offset, len)? else {
        return Ok(HolePunchReport::default());
    };

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &plan.allocator)?;
    store_inode_table(&mut capture, superblock, &plan.inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "hole-punch image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "hole-punch image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("hole-punch image rendered outside allocation and inode regions")?;
    changed.extend(plan.data_writes.iter().copied());

    if changed.is_empty() {
        return Ok(HolePunchReport {
            released_blocks: plan.released_blocks,
            punched_logical_blocks: plan.punched_logical_blocks,
            zeroed_edge_blocks: plan.zeroed_edge_blocks,
            transaction: RecoveryReport::default(),
        });
    }

    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for (block, image) in changed.iter().copied() {
        log.write(txid, block, image)?;
    }
    log.commit(txid)?;
    store_journal_image(device, *superblock, log.entries())?;

    let transaction = recover_journal_and_checkpoint(device, *superblock)?;
    if transaction.committed_transactions != 1 || transaction.home_writes != changed.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "hole-punch recovery report is inconsistent",
        ));
    }
    check_device(device)?;

    Ok(HolePunchReport {
        released_blocks: plan.released_blocks,
        punched_logical_blocks: plan.punched_logical_blocks,
        zeroed_edge_blocks: plan.zeroed_edge_blocks,
        transaction,
    })
}

fn prepare_hole_punch(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    offset: u64,
    len: u64,
) -> io::Result<Option<HolePunchPlan>> {
    if len == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hole-punch range must be non-empty",
        ));
    }
    let end = offset.checked_add(len).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "hole-punch byte range overflow")
    })?;

    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let target = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "hole-punch inode is missing"))?;
    if target.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hole-punch target must be a regular file",
        ));
    }
    let byte_len = target
        .canonical_byte_len()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if offset >= byte_len || end > byte_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "hole-punch range extends beyond EOF",
        ));
    }

    let first = usize::try_from(offset / BLOCK_SIZE_U64)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "hole-punch index too large"))?;
    let last = usize::try_from((end - 1) / BLOCK_SIZE_U64)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "hole-punch index too large"))?;

    let mut released_blocks = Vec::new();
    let mut punched_logical_blocks = Vec::new();
    let mut zeroed_edge_blocks = Vec::new();
    let mut data_writes = Vec::new();

    for logical_index in first..=last {
        let logical_u64 = u64::try_from(logical_index)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "logical index too large"))?;
        let block_start = logical_u64.checked_mul(BLOCK_SIZE_U64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "logical byte offset overflow")
        })?;
        let block_end = block_start
            .checked_add(BLOCK_SIZE_U64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "block end overflow"))?;
        let visible_end = block_end.min(byte_len);
        let overlap_start = offset.max(block_start);
        let overlap_end = end.min(visible_end);
        if overlap_start >= overlap_end {
            continue;
        }

        let mapping = *target.blocks.get(logical_index).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "hole-punch EOF exceeds inode logical mapping",
            )
        })?;
        let covers_visible_block = overlap_start == block_start && overlap_end == visible_end;

        if covers_visible_block {
            if mapping != SPARSE_HOLE_BLOCK {
                require_owned(&allocator, mapping)?;
                allocator
                    .free(mapping)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                target.replace_block_range(
                    logical_index..logical_index + 1,
                    &[SPARSE_HOLE_BLOCK],
                )?;
                released_blocks.push(mapping);
                punched_logical_blocks.push(logical_index);
            }
            continue;
        }

        if mapping == SPARSE_HOLE_BLOCK {
            continue;
        }
        require_owned(&allocator, mapping)?;
        let mut image = [0_u8; BLOCK_SIZE];
        device.read_block(mapping, &mut image)?;
        let begin = usize::try_from(overlap_start - block_start).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "hole-punch edge offset too large")
        })?;
        let finish = usize::try_from(overlap_end - block_start).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "hole-punch edge offset too large")
        })?;
        let original = image;
        image[begin..finish].fill(0);
        if image != original {
            data_writes.push((mapping, image));
            zeroed_edge_blocks.push(logical_index);
        }
    }

    released_blocks.sort_unstable();
    punched_logical_blocks.sort_unstable();
    zeroed_edge_blocks.sort_unstable();
    data_writes.sort_unstable_by_key(|(block, _)| *block);

    if released_blocks.is_empty() && data_writes.is_empty() {
        return Ok(None);
    }

    Ok(Some(HolePunchPlan {
        allocator,
        inodes,
        released_blocks,
        punched_logical_blocks,
        zeroed_edge_blocks,
        data_writes,
    }))
}

fn require_owned(allocator: &BlockAllocator, block: u64) -> io::Result<()> {
    let owned = allocator
        .is_owned(block)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !owned {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "hole-punch physical block is not allocator-owned",
        ));
    }
    Ok(())
}
