use std::io;

use crate::allocation_disk::{load_allocator, store_allocator};
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

/// Atomically replaces one non-empty existing logical-block range with a non-empty caller-provided
/// block sequence that may have a different length.
///
/// Fresh physical blocks are allocated before any displaced blocks are released. Allocation metadata,
/// the resized inode block vector, and every replacement data image are then published through one
/// WAL transaction. Namespace metadata and the filesystem format are unchanged.
///
/// Format v5 has no persisted byte length, so this operation is deliberately block-granular. It does
/// not define byte-range replacement, EOF, sparse holes, extents, reflinks, or POSIX splice semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty replacement, a zero-length destination range, a missing or
/// non-file inode, a destination range outside the existing logical blocks, range overflow, or
/// insufficient free blocks. Returns `InvalidData` when allocator ownership disagrees with displaced
/// inode references. Journal-capacity, encoding, recovery, checkpoint, and block-device I/O failures
/// are propagated.
pub fn replace_file_blocks_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    start: usize,
    remove_count: usize,
    replacements: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    if remove_count == 0 || replacements.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "block replacement requires non-empty removed and replacement ranges",
        ));
    }
    let end = start.checked_add(remove_count).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "block replacement destination range overflows usize",
        )
    })?;

    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "replacement inode is missing")
        })?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "block replacement target must be a regular file",
        ));
    }
    if end > inode.blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "block replacement range exceeds existing logical blocks",
        ));
    }

    let displaced_blocks = inode.blocks[start..end].to_vec();
    for block in &displaced_blocks {
        if !allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "replacement displaced block is not allocator-owned",
            ));
        }
    }

    let mut new_blocks = Vec::with_capacity(replacements.len());
    for _ in replacements {
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
    inode.blocks.splice(start..end, new_blocks.iter().copied());

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &allocator)?;
    store_inode_table(&mut capture, superblock, &inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "block replacement image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "block replacement image did not render every inode metadata block",
        &mut changed,
    )?;
    capture
        .ensure_empty("block replacement image rendered outside allocation and inode regions")?;
    changed.extend(new_blocks.iter().copied().zip(replacements.iter().copied()));

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
            "block replacement recovery report is inconsistent",
        ));
    }

    Ok((new_blocks, displaced_blocks, report))
}
