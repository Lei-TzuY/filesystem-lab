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

/// Allocates and inserts multiple complete logical blocks at one regular-file boundary atomically.
///
/// Existing logical blocks at `insert_index` and later shift right by `data_blocks.len()` positions.
/// Allocator ownership, inode block-reference growth, and every new data block are published through
/// one WAL transaction. Namespace metadata and on-disk format are unchanged.
///
/// Format v5 has no persisted byte length, so this API is deliberately block-granular. It does not
/// claim byte-level insertion, EOF, sparse-hole, extent, or POSIX insert-range semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty insertion, a missing/non-file inode, an insertion index beyond
/// the current logical block count, or insufficient free data blocks. Encoding, journal-capacity,
/// checkpoint, and block-device I/O failures are propagated; an inconsistent recovery report is
/// returned as `InvalidData`.
pub fn insert_file_blocks_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
    insert_index: usize,
    data_blocks: &[[u8; BLOCK_SIZE]],
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    if data_blocks.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "multi-block insert requires at least one data block",
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
                "file insert target inode is missing",
            )
        })?;
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

    let mut blocks = Vec::with_capacity(data_blocks.len());
    for _ in data_blocks {
        let block = allocator
            .allocate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        blocks.push(block);
    }
    inode
        .blocks
        .splice(insert_index..insert_index, blocks.iter().copied());

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, &allocator)?;
    store_inode_table(&mut capture, superblock, &inodes)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "multi-block insert image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "multi-block insert image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("multi-block insert image rendered outside allocation and inode regions")?;
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
            "multi-block insert recovery report is inconsistent",
        ));
    }

    Ok((blocks, report))
}
