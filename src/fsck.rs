use std::collections::{BTreeMap, BTreeSet};
use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::load_directory_table;
use crate::format::{read_superblock, Superblock};
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::load_inode_table;
use crate::journal::{JournalEntry, TransactionId};
use crate::journal_region::load_journal_image;

pub const ROOT_INODE_ID: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FsckReport {
    pub total_blocks: u64,
    pub reserved_blocks: u64,
    pub data_blocks: u64,
    pub allocated_blocks: u64,
    pub free_blocks: u64,
    pub inode_records: usize,
    pub referenced_blocks: usize,
    pub directory_entries: usize,
    pub journal_entries: usize,
    pub journal_writes: usize,
    pub committed_transactions: usize,
    pub pending_transaction: Option<TransactionId>,
}

/// Performs a read-only consistency check over all currently durable filesystem metadata.
///
/// The check validates the superblock, allocation bitmap, inode table, directory table, and bounded
/// journal region. It enforces cross-layer ownership and namespace references: every inode block
/// reference must name an allocated data block, no data block may be owned by more than one inode,
/// every directory parent/target must name an existing inode, and every directory parent must itself
/// be a directory. Once the inode table is non-empty, inode 1 is the unique root, must be a directory,
/// every inode must be reachable from it, and the directory subgraph must be acyclic. It never writes
/// or flushes the device.
///
/// An incomplete final journal transaction is reported as `pending_transaction` rather than treated
/// as corruption because it is the expected durable state after a crash before the commit marker.
///
/// # Errors
///
/// Returns `InvalidData` for malformed/corrupt durable metadata, invalid allocation accounting,
/// invalid inode references or double ownership, dangling/invalid directory references, missing or
/// invalid root state, unreachable inodes, directory cycles, malformed journal ordering, or forbidden
/// journal home locations. Underlying device read errors are propagated.
pub fn check_device(device: &mut impl BlockDevice) -> io::Result<FsckReport> {
    let superblock = read_superblock(device).map_err(|error| with_context("superblock", &error))?;
    let allocator =
        load_allocator(device, &superblock).map_err(|error| with_context("allocation", &error))?;
    let inodes = load_inode_table(device, &superblock)
        .map_err(|error| with_context("inode table", &error))?;
    let referenced_blocks = audit_inode_ownership(&superblock, &allocator, &inodes)?;
    let directory_entries = load_directory_table(device, &superblock)
        .map_err(|error| with_context("directory table", &error))?;
    audit_namespace(&inodes, &directory_entries)?;
    let entries =
        load_journal_image(device, superblock).map_err(|error| with_context("journal", &error))?;
    audit_journal(
        superblock,
        &entries,
        allocator.allocated_blocks(),
        allocator.free_blocks(),
        inodes.len(),
        referenced_blocks,
        directory_entries.len(),
    )
}

fn audit_inode_ownership(
    superblock: &Superblock,
    allocator: &BlockAllocator,
    inodes: &[PersistedInode],
) -> io::Result<usize> {
    let reserved_blocks = superblock.reserved_blocks();
    let mut owners = BTreeMap::<u64, u64>::new();
    let mut referenced_blocks = 0_usize;

    for inode in inodes {
        for block in &inode.blocks {
            if *block < reserved_blocks || *block >= superblock.total_blocks {
                return Err(invalid_data_owned(format!(
                    "inode {} references reserved or out-of-range block {}",
                    inode.id, block
                )));
            }
            let allocated = allocator
                .is_owned(*block)
                .map_err(|error| invalid_data_owned(error.to_string()))?;
            if !allocated {
                return Err(invalid_data_owned(format!(
                    "inode {} references unallocated block {}",
                    inode.id, block
                )));
            }
            if let Some(previous) = owners.insert(*block, inode.id) {
                return Err(invalid_data_owned(format!(
                    "block {} is owned by both inode {} and inode {}",
                    block, previous, inode.id
                )));
            }
            referenced_blocks = referenced_blocks
                .checked_add(1)
                .ok_or_else(|| invalid_data("inode reference count overflow"))?;
        }
    }

    Ok(referenced_blocks)
}

fn audit_namespace(
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
) -> io::Result<()> {
    let inode_kinds = inodes
        .iter()
        .map(|inode| (inode.id, inode.kind))
        .collect::<BTreeMap<_, _>>();
    let mut adjacency = BTreeMap::<u64, Vec<u64>>::new();

    for entry in entries {
        let parent_kind = inode_kinds.get(&entry.parent).ok_or_else(|| {
            invalid_data_owned(format!(
                "directory entry '{}' references missing parent inode {}",
                entry.name, entry.parent
            ))
        })?;
        if *parent_kind != InodeKind::Directory {
            return Err(invalid_data_owned(format!(
                "directory entry '{}' parent inode {} is not a directory",
                entry.name, entry.parent
            )));
        }
        if !inode_kinds.contains_key(&entry.target) {
            return Err(invalid_data_owned(format!(
                "directory entry '{}' references missing target inode {}",
                entry.name, entry.target
            )));
        }
        adjacency
            .entry(entry.parent)
            .or_default()
            .push(entry.target);
    }

    if inodes.is_empty() {
        return Ok(());
    }

    let root_kind = inode_kinds
        .get(&ROOT_INODE_ID)
        .ok_or_else(|| invalid_data("non-empty inode table is missing root inode 1"))?;
    if *root_kind != InodeKind::Directory {
        return Err(invalid_data("root inode 1 is not a directory"));
    }

    let mut states = BTreeMap::<u64, u8>::new();
    for inode in inodes
        .iter()
        .filter(|inode| inode.kind == InodeKind::Directory)
    {
        audit_directory_cycle(inode.id, &inode_kinds, &adjacency, &mut states)?;
    }

    let mut reachable = BTreeSet::new();
    let mut pending = vec![ROOT_INODE_ID];
    while let Some(inode) = pending.pop() {
        if !reachable.insert(inode) {
            continue;
        }
        if let Some(targets) = adjacency.get(&inode) {
            pending.extend(targets.iter().copied());
        }
    }

    if let Some(unreachable) = inodes.iter().find(|inode| !reachable.contains(&inode.id)) {
        return Err(invalid_data_owned(format!(
            "inode {} is unreachable from root inode {}",
            unreachable.id, ROOT_INODE_ID
        )));
    }

    Ok(())
}

fn audit_directory_cycle(
    inode: u64,
    inode_kinds: &BTreeMap<u64, InodeKind>,
    adjacency: &BTreeMap<u64, Vec<u64>>,
    states: &mut BTreeMap<u64, u8>,
) -> io::Result<()> {
    match states.get(&inode).copied() {
        Some(1) => {
            return Err(invalid_data_owned(format!(
                "directory cycle includes inode {inode}"
            )))
        }
        Some(2) => return Ok(()),
        _ => {}
    }

    states.insert(inode, 1);
    if let Some(targets) = adjacency.get(&inode) {
        for target in targets {
            if inode_kinds.get(target) == Some(&InodeKind::Directory) {
                audit_directory_cycle(*target, inode_kinds, adjacency, states)?;
            }
        }
    }
    states.insert(inode, 2);
    Ok(())
}

fn audit_journal(
    superblock: Superblock,
    entries: &[JournalEntry],
    allocated_blocks: u64,
    free_blocks: u64,
    inode_records: usize,
    referenced_blocks: usize,
    directory_entries: usize,
) -> io::Result<FsckReport> {
    let reserved_blocks = superblock.reserved_blocks();
    let data_blocks = superblock
        .total_blocks
        .checked_sub(reserved_blocks)
        .ok_or_else(|| invalid_data("reserved metadata exceeds filesystem size"))?;
    if allocated_blocks
        .checked_add(free_blocks)
        .ok_or_else(|| invalid_data("allocation accounting overflow"))?
        != data_blocks
    {
        return Err(invalid_data("allocation accounting mismatch"));
    }

    let mut active = None;
    let mut journal_writes = 0_usize;
    let mut committed_transactions = 0_usize;

    for entry in entries {
        match entry {
            JournalEntry::Begin { txid } => {
                if active.is_some() {
                    return Err(invalid_data("nested journal transaction"));
                }
                active = Some(*txid);
            }
            JournalEntry::Write { txid, block, .. } => {
                if active != Some(*txid) {
                    return Err(invalid_data(
                        "journal write does not match active transaction",
                    ));
                }
                let allocation_home = superblock.allocation_range().contains(block);
                let inode_home = superblock.inode_range().contains(block);
                let directory_home = superblock.directory_range().contains(block);
                let data_home = *block >= reserved_blocks && *block < superblock.total_blocks;
                if !allocation_home && !inode_home && !directory_home && !data_home {
                    return Err(invalid_data(
                        "journal write targets forbidden or invalid block",
                    ));
                }
                journal_writes = journal_writes
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("journal write count overflow"))?;
            }
            JournalEntry::Commit { txid } => {
                if active != Some(*txid) {
                    return Err(invalid_data(
                        "journal commit does not match active transaction",
                    ));
                }
                active = None;
                committed_transactions = committed_transactions
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("committed transaction count overflow"))?;
            }
        }
    }

    Ok(FsckReport {
        total_blocks: superblock.total_blocks,
        reserved_blocks,
        data_blocks,
        allocated_blocks,
        free_blocks,
        inode_records,
        referenced_blocks,
        directory_entries,
        journal_entries: entries.len(),
        journal_writes,
        committed_transactions,
        pending_transaction: active,
    })
}

fn with_context(layer: &'static str, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("fsck {layer}: {error}"))
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_data_owned(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocation::BlockAllocator;
    use crate::block::BLOCK_SIZE;

    #[test]
    fn audit_reports_committed_and_pending_transactions() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let data = Box::new([7_u8; BLOCK_SIZE]);
        let entries = vec![
            JournalEntry::Begin { txid: 1 },
            JournalEntry::Write {
                txid: 1,
                block: superblock.reserved_blocks(),
                data: data.clone(),
            },
            JournalEntry::Commit { txid: 1 },
            JournalEntry::Begin { txid: 2 },
            JournalEntry::Write {
                txid: 2,
                block: superblock.reserved_blocks() + 1,
                data,
            },
        ];
        let data_blocks = superblock.total_blocks - superblock.reserved_blocks();

        let report = audit_journal(superblock, &entries, 0, data_blocks, 0, 0, 0).unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.pending_transaction, Some(2));
        assert_eq!(report.journal_writes, 2);
        assert_eq!(report.data_blocks, data_blocks);
        assert_eq!(report.allocated_blocks, 0);
        assert_eq!(report.free_blocks, data_blocks);
        assert_eq!(report.directory_entries, 0);
    }

    #[test]
    fn audit_accepts_metadata_regions_as_journal_home() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let entries = vec![
            JournalEntry::Begin { txid: 1 },
            JournalEntry::Write {
                txid: 1,
                block: superblock.allocation_start,
                data: Box::new([0_u8; BLOCK_SIZE]),
            },
            JournalEntry::Write {
                txid: 1,
                block: superblock.inode_start,
                data: Box::new([0_u8; BLOCK_SIZE]),
            },
            JournalEntry::Write {
                txid: 1,
                block: superblock.directory_start,
                data: Box::new([0_u8; BLOCK_SIZE]),
            },
            JournalEntry::Commit { txid: 1 },
        ];
        let data_blocks = superblock.total_blocks - superblock.reserved_blocks();

        let report = audit_journal(superblock, &entries, 0, data_blocks, 0, 0, 0).unwrap();
        assert_eq!(report.journal_writes, 3);
        assert_eq!(report.committed_transactions, 1);
    }

    #[test]
    fn audit_rejects_superblock_and_journal_ownership() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let data_blocks = superblock.total_blocks - superblock.reserved_blocks();

        for forbidden in [0, superblock.journal_start] {
            let entries = vec![
                JournalEntry::Begin { txid: 1 },
                JournalEntry::Write {
                    txid: 1,
                    block: forbidden,
                    data: Box::new([0_u8; BLOCK_SIZE]),
                },
            ];

            assert_eq!(
                audit_journal(superblock, &entries, 0, data_blocks, 0, 0, 0)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn inode_ownership_requires_allocated_unique_data_blocks() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let mut allocator =
            BlockAllocator::new(superblock.total_blocks, superblock.reserved_blocks()).unwrap();
        let block = allocator.allocate().unwrap();
        let inodes = vec![PersistedInode {
            id: 1,
            kind: InodeKind::File,
            blocks: vec![block],
        }];
        assert_eq!(
            audit_inode_ownership(&superblock, &allocator, &inodes).unwrap(),
            1
        );

        let duplicate = vec![
            inodes[0].clone(),
            PersistedInode {
                id: 2,
                kind: InodeKind::Directory,
                blocks: vec![block],
            },
        ];
        assert_eq!(
            audit_inode_ownership(&superblock, &allocator, &duplicate)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn namespace_requires_existing_directory_parent_and_target() {
        let inodes = vec![
            PersistedInode {
                id: 1,
                kind: InodeKind::Directory,
                blocks: vec![],
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::File,
                blocks: vec![],
            },
        ];
        let entry = PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "child".to_owned(),
        };
        audit_namespace(&inodes, std::slice::from_ref(&entry)).unwrap();

        let missing_parent = PersistedDirectoryEntry {
            parent: 3,
            ..entry.clone()
        };
        assert_eq!(
            audit_namespace(&inodes, &[missing_parent])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );

        let missing_target = PersistedDirectoryEntry { target: 3, ..entry };
        assert_eq!(
            audit_namespace(&inodes, &[missing_target])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn namespace_requires_directory_root_and_full_reachability() {
        let root = PersistedInode {
            id: ROOT_INODE_ID,
            kind: InodeKind::Directory,
            blocks: vec![],
        };
        let file = PersistedInode {
            id: 2,
            kind: InodeKind::File,
            blocks: vec![],
        };

        let missing_root = vec![file.clone()];
        assert!(audit_namespace(&missing_root, &[])
            .unwrap_err()
            .to_string()
            .contains("missing root inode 1"));

        let invalid_root = vec![PersistedInode {
            id: ROOT_INODE_ID,
            kind: InodeKind::File,
            blocks: vec![],
        }];
        assert!(audit_namespace(&invalid_root, &[])
            .unwrap_err()
            .to_string()
            .contains("root inode 1 is not a directory"));

        assert!(audit_namespace(&[root.clone(), file.clone()], &[])
            .unwrap_err()
            .to_string()
            .contains("inode 2 is unreachable"));

        let link = PersistedDirectoryEntry {
            parent: ROOT_INODE_ID,
            target: file.id,
            name: "file".to_owned(),
        };
        audit_namespace(&[root, file], &[link]).unwrap();
    }

    #[test]
    fn namespace_rejects_directory_cycles() {
        let inodes = vec![
            PersistedInode {
                id: ROOT_INODE_ID,
                kind: InodeKind::Directory,
                blocks: vec![],
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::Directory,
                blocks: vec![],
            },
        ];
        let entries = vec![
            PersistedDirectoryEntry {
                parent: ROOT_INODE_ID,
                target: 2,
                name: "child".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 2,
                target: ROOT_INODE_ID,
                name: "back".to_owned(),
            },
        ];

        assert!(audit_namespace(&inodes, &entries)
            .unwrap_err()
            .to_string()
            .contains("directory cycle"));
    }
}
