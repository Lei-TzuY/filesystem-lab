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

/// Allocates and inserts one complete block at an existing regular file's logical block boundary.
///
/// Existing logical blocks at `insert_index` and later shift right by one position. The new
/// allocator ownership, inode block-reference vector, and data block are committed in one WAL
/// transaction. Namespace metadata and on-disk format are unchanged.
///
/// Format v5 has no persisted byte length, so this is deliberately block-granular. It does not
/// claim byte-level insertion, EOF, sparse-hole, or extent semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for a missing/non-file inode, an insertion index greater than the
/// existing block count, or allocator exhaustion. Encoding, journal-capacity, checkpoint, and
/// block-device I/O failures are propagated; an inconsistent recovery report is `InvalidData`.
pub fn insert_file_block_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    insert_index: usize,
    data: [u8; BLOCK_SIZE],
) -> io::Result<(u64, RecoveryReport)> {
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter_mut()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "file insert target inode is missing"))?;
    if inode.kind != InodeKind::File {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file insert target must be a regular file",
        ));
    }
    if insert_index > inode.blocks.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "file insert logical index is beyond the end",
        ));
    }

    let block = allocator
        .allocate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    inode.blocks.insert(insert_index, block);

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &allocator)?;
    store_inode_table(&mut capture, superblock, &inodes)?;
    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "file insert image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "file insert image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("file insert image rendered outside allocation and inode regions")?;
    changed.push((block, data));

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
            "file insert recovery report is inconsistent",
        ));
    }
    Ok((block, report))
}
