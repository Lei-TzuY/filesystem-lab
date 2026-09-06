use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::{load_allocator, store_allocator};
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileBlockRange {
    pub inode: u64,
    pub start: usize,
    pub block_count: usize,
}

fn range_end(range: FileBlockRange, label: &'static str) -> io::Result<usize> {
    if range.block_count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone splice requires non-empty source and destination ranges",
        ));
    }
    range.start.checked_add(range.block_count).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("clone splice {label} range overflows usize"),
        )
    })
}

fn inode_index(inodes: &[PersistedInode], inode_id: u64, label: &'static str) -> io::Result<usize> {
    inodes
        .iter()
        .position(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("clone {label} inode is missing"),
            )
        })
}

fn snapshot_owned_blocks(
    device: &mut impl BlockDevice,
    allocator: &BlockAllocator,
    blocks: &[u64],
    message: &'static str,
) -> io::Result<Vec<[u8; BLOCK_SIZE]>> {
    let mut snapshots = Vec::with_capacity(blocks.len());
    for block in blocks {
        if !allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, message));
        }
        let mut image = [0_u8; BLOCK_SIZE];
        device.read_block(*block, &mut image)?;
        snapshots.push(image);
    }
    Ok(snapshots)
}

fn publish_splice(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    allocator: &BlockAllocator,
    inodes: &[PersistedInode],
    new_blocks: &[u64],
    snapshots: &[[u8; BLOCK_SIZE]],
) -> io::Result<RecoveryReport> {
    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, allocator)?;
    store_inode_table(&mut capture, superblock, inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "clone splice image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "clone splice image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("clone splice image rendered outside allocation and inode regions")?;
    changed.extend(new_blocks.iter().copied().zip(snapshots.iter().copied()));

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
            "clone splice recovery report is inconsistent",
        ));
    }
    Ok(report)
}

fn validate_ranges(
    inodes: &[PersistedInode],
    source: FileBlockRange,
    destination: FileBlockRange,
) -> io::Result<(usize, usize, usize, usize)> {
    if source.inode == destination.inode {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone splice requires distinct source and destination inodes",
        ));
    }
    let source_end = range_end(source, "source")?;
    let destination_end = range_end(destination, "destination")?;
    let source_index = inode_index(inodes, source.inode, "source")?;
    let destination_index = inode_index(inodes, destination.inode, "destination")?;
    if inodes[source_index].kind != InodeKind::File
        || inodes[destination_index].kind != InodeKind::File
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone source and destination must be regular files",
        ));
    }
    if source_end > inodes[source_index].blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone source range exceeds existing logical blocks",
        ));
    }
    if destination_end > inodes[destination_index].blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clone destination range exceeds existing logical blocks",
        ));
    }
    Ok((source_index, source_end, destination_index, destination_end))
}

/// Replaces an existing destination logical-block range with a differently sized fresh clone range.
///
/// Source images are snapshotted before metadata mutation. Fresh physical blocks are allocated while
/// every displaced destination block remains owned, then exactly the displaced homes are released.
/// Allocation metadata, the resized destination block vector, and all cloned data homes are published
/// through one WAL transaction. Source and destination must be distinct regular-file inodes.
///
/// Format v5 has no persisted byte length, so this is deliberately block-granular. It does not define
/// EOF, partial-block replacement, sparse holes, extents, reflinks, or POSIX splice semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for empty source/destination ranges, identical endpoints, missing/non-file
/// inodes, ranges outside existing logical blocks, range overflow, or insufficient free blocks.
/// Returns `InvalidData` when allocator ownership disagrees with selected references. Journal-capacity,
/// encoding, recovery, checkpoint, and block-device I/O failures are propagated.
pub fn clone_file_blocks_splice_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: FileBlockRange,
    destination: FileBlockRange,
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let (source_index, source_end, destination_index, destination_end) =
        validate_ranges(&inodes, source, destination)?;

    let source_blocks = inodes[source_index].blocks[source.start..source_end].to_vec();
    let displaced_blocks =
        inodes[destination_index].blocks[destination.start..destination_end].to_vec();
    let snapshots = snapshot_owned_blocks(
        device,
        &allocator,
        &source_blocks,
        "clone source block is not allocator-owned",
    )?;
    let _ = snapshot_owned_blocks(
        device,
        &allocator,
        &displaced_blocks,
        "clone destination block is not allocator-owned",
    )?;

    let mut new_blocks = Vec::with_capacity(source.block_count);
    for _ in 0..source.block_count {
        let block = allocator
            .allocate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        new_blocks.push(block);
    }
    for block in &displaced_blocks {
        allocator
            .free(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }
    inodes[destination_index].blocks.splice(
        destination.start..destination_end,
        new_blocks.iter().copied(),
    );

    let report = publish_splice(
        device,
        superblock,
        &allocator,
        &inodes,
        &new_blocks,
        &snapshots,
    )?;
    Ok((new_blocks, displaced_blocks, report))
}
