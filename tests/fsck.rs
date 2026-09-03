use std::io;

use filesystem_lab::allocation::BlockAllocator;
use filesystem_lab::allocation_disk::store_allocator;
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::format_device;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::journal::JournalLog;
use filesystem_lab::journal_region::store_journal_image;

#[derive(Debug)]
struct MemoryDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
    writes: usize,
    flushes: usize,
}

impl MemoryDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0_u8; BLOCK_SIZE]; blocks],
            writes: 0,
            flushes: 0,
        }
    }
}

impl BlockDevice for MemoryDevice {
    fn block_count(&self) -> u64 {
        u64::try_from(self.blocks.len()).expect("test device size fits u64")
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
        let source = self
            .blocks
            .get(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
        *buf = *source;
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
        let destination = self
            .blocks
            .get_mut(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
        *destination = *buf;
        self.writes += 1;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn fresh_device_passes_without_mutation() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let writes_before = device.writes;
    let flushes_before = device.flushes;

    let report = check_device(&mut device).unwrap();
    let data_blocks = superblock.total_blocks - superblock.reserved_blocks();

    assert_eq!(report.total_blocks, 16);
    assert_eq!(report.reserved_blocks, superblock.reserved_blocks());
    assert_eq!(report.data_blocks, data_blocks);
    assert_eq!(report.allocated_blocks, 0);
    assert_eq!(report.free_blocks, data_blocks);
    assert_eq!(report.inode_records, 0);
    assert_eq!(report.referenced_blocks, 0);
    assert_eq!(report.directory_entries, 0);
    assert_eq!(report.journal_entries, 0);
    assert_eq!(report.journal_writes, 0);
    assert_eq!(report.committed_transactions, 0);
    assert_eq!(report.pending_transaction, None);
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
}

#[test]
fn accepts_inode_references_that_match_durable_allocation() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator =
        BlockAllocator::new(superblock.total_blocks, superblock.reserved_blocks()).unwrap();
    let block = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            PersistedInode {
                id: 1,
                kind: InodeKind::Directory,
                blocks: vec![],
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::File,
                blocks: vec![block],
            },
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "file".to_owned(),
        }],
    )
    .unwrap();

    let writes_before = device.writes;
    let flushes_before = device.flushes;
    let report = check_device(&mut device).unwrap();

    assert_eq!(report.allocated_blocks, 1);
    assert_eq!(report.inode_records, 2);
    assert_eq!(report.referenced_blocks, 1);
    assert_eq!(report.directory_entries, 1);
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
}

#[test]
fn accepts_durable_namespace_with_existing_directory_parent_and_target() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
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
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "child".to_owned(),
        }],
    )
    .unwrap();

    let writes_before = device.writes;
    let flushes_before = device.flushes;
    let report = check_device(&mut device).unwrap();

    assert_eq!(report.inode_records, 2);
    assert_eq!(report.directory_entries, 1);
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
}

#[test]
fn rejects_directory_entry_with_missing_parent() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[PersistedInode {
            id: 2,
            kind: InodeKind::File,
            blocks: vec![],
        }],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "child".to_owned(),
        }],
    )
    .unwrap();

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("missing parent inode 1"));
}

#[test]
fn rejects_directory_entry_with_missing_target() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: vec![],
        }],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "child".to_owned(),
        }],
    )
    .unwrap();

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("missing target inode 2"));
}

#[test]
fn rejects_directory_entry_whose_parent_is_not_a_directory() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            PersistedInode {
                id: 1,
                kind: InodeKind::File,
                blocks: vec![],
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::File,
                blocks: vec![],
            },
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "child".to_owned(),
        }],
    )
    .unwrap();

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains("parent inode 1 is not a directory"));
}

#[test]
fn rejects_nonempty_inode_table_without_directory_root() {
    for inodes in [
        vec![PersistedInode {
            id: 2,
            kind: InodeKind::File,
            blocks: vec![],
        }],
        vec![PersistedInode {
            id: 1,
            kind: InodeKind::File,
            blocks: vec![],
        }],
    ] {
        let mut device = MemoryDevice::new(16);
        let superblock = format_device(&mut device).unwrap();
        store_inode_table(&mut device, &superblock, &inodes).unwrap();

        let error = check_device(&mut device).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("root inode 1"));
    }
}

#[test]
fn rejects_unreachable_inode_from_root() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
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
        ],
    )
    .unwrap();

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("inode 2 is unreachable from root inode 1"));
}

#[test]
fn rejects_directory_cycle_even_when_all_inodes_are_reachable() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            PersistedInode {
                id: 1,
                kind: InodeKind::Directory,
                blocks: vec![],
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::Directory,
                blocks: vec![],
            },
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            PersistedDirectoryEntry {
                parent: 1,
                target: 2,
                name: "child".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 2,
                target: 1,
                name: "back".to_owned(),
            },
        ],
    )
    .unwrap();

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("directory cycle"));
}

#[test]
fn rejects_inode_reference_to_unallocated_block() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[PersistedInode {
            id: 7,
            kind: InodeKind::File,
            blocks: vec![superblock.reserved_blocks()],
        }],
    )
    .unwrap();

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("references unallocated block"));
}

#[test]
fn rejects_cross_inode_double_ownership() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator =
        BlockAllocator::new(superblock.total_blocks, superblock.reserved_blocks()).unwrap();
    let block = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            PersistedInode {
                id: 1,
                kind: InodeKind::File,
                blocks: vec![block],
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::Directory,
                blocks: vec![block],
            },
        ],
    )
    .unwrap();

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("owned by both inode"));
}

#[test]
fn rejects_reserved_and_out_of_range_inode_references() {
    for bad_block in [0, 16] {
        let mut device = MemoryDevice::new(16);
        let superblock = format_device(&mut device).unwrap();
        store_inode_table(
            &mut device,
            &superblock,
            &[PersistedInode {
                id: 3,
                kind: InodeKind::File,
                blocks: vec![bad_block],
            }],
        )
        .unwrap();

        let error = check_device(&mut device).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error
            .to_string()
            .contains("references reserved or out-of-range block"));
    }
}

#[test]
fn reports_committed_and_crash_incomplete_transactions() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let mut log = JournalLog::new();

    let committed = log.begin().unwrap();
    log.write(committed, superblock.reserved_blocks(), [0x11; BLOCK_SIZE])
        .unwrap();
    log.commit(committed).unwrap();

    let pending = log.begin().unwrap();

    store_journal_image(&mut device, superblock, log.entries()).unwrap();
    let writes_before = device.writes;
    let flushes_before = device.flushes;

    let report = check_device(&mut device).unwrap();

    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.pending_transaction, Some(pending));
    assert_eq!(report.journal_writes, 1);
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
}

#[test]
fn accepts_inode_table_as_journal_home() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let mut log = JournalLog::new();
    let txid = log.begin().unwrap();
    log.write(txid, superblock.inode_start, [0_u8; BLOCK_SIZE])
        .unwrap();
    log.commit(txid).unwrap();
    store_journal_image(&mut device, superblock, log.entries()).unwrap();

    let report = check_device(&mut device).unwrap();
    assert_eq!(report.journal_writes, 1);
    assert_eq!(report.committed_transactions, 1);
}

#[test]
fn detects_superblock_corruption_before_journal_scan() {
    let mut device = MemoryDevice::new(16);
    format_device(&mut device).unwrap();
    device.blocks[0][BLOCK_SIZE - 1] = 1;

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("fsck superblock"));
}

#[test]
fn detects_allocation_corruption_before_journal_scan() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let allocation_block = usize::try_from(superblock.allocation_start).unwrap();
    device.blocks[allocation_block][32] ^= 0x80;

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("fsck allocation"));
}

#[test]
fn detects_inode_table_corruption_before_journal_scan() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let inode_block = usize::try_from(superblock.inode_start).unwrap();
    device.blocks[inode_block][8] ^= 0x80;

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("fsck inode table"));
}

#[test]
fn detects_directory_table_corruption_before_journal_scan() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let directory_block = usize::try_from(superblock.directory_start).unwrap();
    device.blocks[directory_block][8] ^= 0x80;

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("fsck directory table"));
}

#[test]
fn detects_cross_block_journal_corruption() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let mut log = JournalLog::new();
    let txid = log.begin().unwrap();
    log.write(txid, superblock.reserved_blocks(), [0x5a; BLOCK_SIZE])
        .unwrap();
    log.commit(txid).unwrap();
    store_journal_image(&mut device, superblock, log.entries()).unwrap();

    let second_journal_block = usize::try_from(superblock.journal_start + 1).unwrap();
    device.blocks[second_journal_block][128] ^= 0xff;

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("fsck journal"));
}
