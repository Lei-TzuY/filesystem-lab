use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::{load_allocator, store_allocator};
use crate::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};
use crate::file_data::write_file_range_journaled;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

/// Atomically writes a byte range and extends the regular file when the write crosses persisted EOF.
///
/// Writes wholly inside the current EOF delegate to the existing overwrite-only range transaction.
/// Extending writes allocate only the additional trailing blocks required by the new EOF. When the
/// write starts after EOF, every byte in the intervening non-sparse gap is explicitly zero-filled.
/// The payload, any zero-filled gap/tail bytes, allocator ownership, block mapping, and exact EOF are
/// published in one bounded WAL transaction.
///
/// This is non-sparse extending-write semantics: no hole representation is created.
///
/// The returned block vector contains blocks newly allocated by an extending write.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty write, missing/non-file target, offset arithmetic overflow,
/// allocator exhaustion, or an unrepresentable target block count. Returns `InvalidData` for
/// allocator/inode disagreement or an inconsistent recovery report. Journal-capacity, encoding,
/// recovery/checkpoint, and block-device failures propagate.
pub fn write_file_range_extending_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    start_offset: u64,
    data: &[u8],
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    if data.is_empty() {
        return Err(invalid_input("extending write requires non-empty data"));
    }
    let data_len = u64::try_from(data.len())
        .map_err(|_| invalid_input("extending write length exceeds u64"))?;
    let end_offset = start_offset
        .checked_add(data_len)
        .ok_or_else(|| invalid_input("extending write end offset overflow"))?;

    match prepare_write_plan(device, superblock, inode_id, start_offset, end_offset, data)? {
        WritePlan::Overwrite {
            first_block_index,
            block_offset,
        } => {
            let report = write_file_range_journaled(
                device,
                superblock,
                inode_id,
                first_block_index,
                block_offset,
                data,
            )?;
            Ok((Vec::new(), report))
        }
        WritePlan::Extend(plan) => {
            let changed = render_changed_homes(device, superblock, &plan)?;
            let report = publish_extending_write(device, *superblock, &changed)?;
            Ok((plan.allocated, report))
        }
    }
}

enum WritePlan {
    Overwrite {
        first_block_index: usize,
        block_offset: usize,
    },
    Extend(ExtendingWritePlan),
}

struct ExtendingWritePlan {
    allocator: BlockAllocator,
    inodes: Vec<PersistedInode>,
    allocated: Vec<u64>,
    data_writes: Vec<(u64, [u8; BLOCK_SIZE])>,
}

#[derive(Clone, Copy)]
struct WriteGeometry {
    current_blocks: usize,
    current_eof: u64,
    start_offset: u64,
    end_offset: u64,
}

fn prepare_write_plan(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    start_offset: u64,
    end_offset: u64,
    data: &[u8],
) -> io::Result<WritePlan> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let inode_index = inodes
        .iter()
        .position(|inode| inode.id == inode_id)
        .ok_or_else(|| invalid_input("extending write target inode is missing"))?;
    if inodes[inode_index].kind != InodeKind::File {
        return Err(invalid_input(
            "extending write target must be a regular file",
        ));
    }

    let current_eof = inodes[inode_index]
        .canonical_byte_len()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if end_offset <= current_eof {
        return Ok(WritePlan::Overwrite {
            first_block_index: usize::try_from(start_offset / BLOCK_SIZE_U64)
                .map_err(|_| invalid_input("extending write logical block index exceeds usize"))?,
            block_offset: usize::try_from(start_offset % BLOCK_SIZE_U64)
                .map_err(|_| invalid_input("extending write block offset exceeds usize"))?,
        });
    }

    let target_blocks = byte_len_to_block_count(end_offset)?;
    let current_blocks = inodes[inode_index].blocks.len();
    if target_blocks < current_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extending write target requires fewer blocks than current inode mapping",
        ));
    }

    let mut allocated = Vec::with_capacity(target_blocks - current_blocks);
    for _ in current_blocks..target_blocks {
        let block = allocator
            .allocate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        allocated.push(block);
    }
    if !allocated.is_empty() {
        inodes[inode_index].replace_block_range(current_blocks..current_blocks, &allocated)?;
    }
    inodes[inode_index].set_file_byte_len(end_offset)?;

    let data_writes = prepare_data_writes(
        device,
        &allocator,
        &inodes[inode_index],
        WriteGeometry {
            current_blocks,
            current_eof,
            start_offset,
            end_offset,
        },
        data,
    )?;

    Ok(WritePlan::Extend(ExtendingWritePlan {
        allocator,
        inodes,
        allocated,
        data_writes,
    }))
}

fn prepare_data_writes(
    device: &mut impl BlockDevice,
    allocator: &BlockAllocator,
    inode: &PersistedInode,
    geometry: WriteGeometry,
    data: &[u8],
) -> io::Result<Vec<(u64, [u8; BLOCK_SIZE])>> {
    let first_changed_byte = geometry.current_eof.min(geometry.start_offset);
    let first_changed_block = usize::try_from(first_changed_byte / BLOCK_SIZE_U64)
        .map_err(|_| invalid_input("extending write first block exceeds usize"))?;
    let end_block = byte_len_to_block_count(geometry.end_offset)?;
    let mut writes = Vec::with_capacity(end_block.saturating_sub(first_changed_block));

    for logical_index in first_changed_block..end_block {
        let physical_block = inode.blocks[logical_index];
        let (mut image, current_image) = load_candidate_block(
            device,
            allocator,
            physical_block,
            logical_index,
            geometry.current_blocks,
        )?;

        let block_start = u64::try_from(logical_index)
            .map_err(|_| invalid_input("extending write block index exceeds u64"))?
            .checked_mul(BLOCK_SIZE_U64)
            .ok_or_else(|| invalid_input("extending write block byte offset overflow"))?;
        let block_end = block_start
            .checked_add(BLOCK_SIZE_U64)
            .ok_or_else(|| invalid_input("extending write block end overflow"))?;

        if geometry.start_offset > geometry.current_eof {
            zero_intersection(
                &mut image,
                block_start,
                block_end,
                geometry.current_eof,
                geometry.start_offset,
            )?;
        }
        copy_intersection(
            &mut image,
            block_start,
            block_end,
            geometry.start_offset,
            geometry.end_offset,
            data,
        )?;
        if geometry.end_offset < block_end {
            zero_intersection(
                &mut image,
                block_start,
                block_end,
                geometry.end_offset,
                block_end,
            )?;
        }

        if current_image != Some(image) {
            writes.push((physical_block, image));
        }
    }
    Ok(writes)
}

fn load_candidate_block(
    device: &mut impl BlockDevice,
    allocator: &BlockAllocator,
    physical_block: u64,
    logical_index: usize,
    current_blocks: usize,
) -> io::Result<([u8; BLOCK_SIZE], Option<[u8; BLOCK_SIZE]>)> {
    if logical_index >= current_blocks {
        return Ok(([0_u8; BLOCK_SIZE], None));
    }

    let owned = allocator
        .is_owned(physical_block)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !owned {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "extending write existing block is not allocator-owned",
        ));
    }
    let mut current = [0_u8; BLOCK_SIZE];
    device.read_block(physical_block, &mut current)?;
    Ok((current, Some(current)))
}

fn render_changed_homes(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    plan: &ExtendingWritePlan,
) -> io::Result<Vec<(u64, [u8; BLOCK_SIZE])>> {
    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &plan.allocator)?;
    store_inode_table(&mut capture, superblock, &plan.inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "extending write image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "extending write image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("extending write image rendered outside allocation and inode regions")?;
    changed.extend(plan.data_writes.iter().copied());
    Ok(changed)
}

fn publish_extending_write(
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
            "extending write recovery report is inconsistent",
        ));
    }
    Ok(report)
}

fn byte_len_to_block_count(byte_len: u64) -> io::Result<usize> {
    let blocks = byte_len.div_ceil(BLOCK_SIZE_U64);
    usize::try_from(blocks)
        .map_err(|_| invalid_input("extending write target block count exceeds usize"))
}

fn zero_intersection(
    image: &mut [u8; BLOCK_SIZE],
    block_start: u64,
    block_end: u64,
    range_start: u64,
    range_end: u64,
) -> io::Result<()> {
    let start = block_start.max(range_start);
    let end = block_end.min(range_end);
    if start >= end {
        return Ok(());
    }
    let local_start = usize::try_from(start - block_start)
        .map_err(|_| invalid_input("zero-fill start exceeds usize"))?;
    let local_end = usize::try_from(end - block_start)
        .map_err(|_| invalid_input("zero-fill end exceeds usize"))?;
    image[local_start..local_end].fill(0);
    Ok(())
}

fn copy_intersection(
    image: &mut [u8; BLOCK_SIZE],
    block_start: u64,
    block_end: u64,
    write_start: u64,
    write_end: u64,
    data: &[u8],
) -> io::Result<()> {
    let start = block_start.max(write_start);
    let end = block_end.min(write_end);
    if start >= end {
        return Ok(());
    }
    let local_start = usize::try_from(start - block_start)
        .map_err(|_| invalid_input("payload block offset exceeds usize"))?;
    let local_end = usize::try_from(end - block_start)
        .map_err(|_| invalid_input("payload block end exceeds usize"))?;
    let data_start = usize::try_from(start - write_start)
        .map_err(|_| invalid_input("payload source offset exceeds usize"))?;
    let data_end = usize::try_from(end - write_start)
        .map_err(|_| invalid_input("payload source end exceeds usize"))?;
    image[local_start..local_end].copy_from_slice(&data[data_start..data_end]);
    Ok(())
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
