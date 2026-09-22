use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal::JournalLog;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint_checked;
use filesystem_lab::journal_region::{load_journal_image, store_journal_image};
use filesystem_lab::path_metadata::metadata_at_path;
use filesystem_lab::recovery::RecoveryReport;

#[derive(Clone, Debug)]
struct MemoryDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
    writes: usize,
    flushes: usize,
}

impl MemoryDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0; BLOCK_SIZE]; blocks],
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
        *buf = *self
            .blocks
            .get(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
        *self
            .blocks
            .get_mut(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))? = *buf;
        self.writes += 1;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

fn block_image(device: &mut MemoryDevice, block: u64) -> [u8; BLOCK_SIZE] {
    let mut data = [0; BLOCK_SIZE];
    device.read_block(block, &mut data).unwrap();
    data
}

fn publish_root_inode(device: &mut MemoryDevice, superblock: Superblock) {
    let mut desired = device.clone();
    let root = PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    
        byte_len: 0,
    };
    store_inode_table(&mut desired, &superblock, std::slice::from_ref(&root)).unwrap();

    let mut log = JournalLog::new();
    let txid = log.begin().unwrap();
    log.write(
        txid,
        superblock.inode_start,
        block_image(&mut desired, superblock.inode_start),
    )
    .unwrap();
    log.commit(txid).unwrap();
    store_journal_image(device, superblock, log.entries()).unwrap();
}

fn publish_orphan_allocation(device: &mut MemoryDevice, superblock: Superblock) -> u64 {
    let mut desired = device.clone();
    let mut allocator = load_allocator(&mut desired, &superblock).unwrap();
    let orphan = allocator.allocate().unwrap();
    store_allocator(&mut desired, &superblock, &allocator).unwrap();

    let mut log = JournalLog::new();
    let txid = log.begin().unwrap();
    log.write(
        txid,
        superblock.allocation_start,
        block_image(&mut desired, superblock.allocation_start),
    )
    .unwrap();
    log.commit(txid).unwrap();
    store_journal_image(device, superblock, log.entries()).unwrap();
    orphan
}

#[test]
fn checked_recovery_installs_valid_projection_and_checkpoints() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    publish_root_inode(&mut device, superblock);

    let report = recover_journal_and_checkpoint_checked(&mut device, superblock).unwrap();

    assert_eq!(
        report,
        RecoveryReport {
            committed_transactions: 1,
            home_writes: 1,
        }
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        vec![PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        
            byte_len: 0,
        }]
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn checked_recovery_is_a_noop_when_no_journal_needs_replay() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    let writes_before = device.writes;
    let flushes_before = device.flushes;

    let report = recover_journal_and_checkpoint_checked(&mut device, superblock).unwrap();

    assert_eq!(report, RecoveryReport::default());
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
}

#[test]
fn checked_recovery_rejects_semantic_corruption_before_any_mutation() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let orphan = publish_orphan_allocation(&mut device, superblock);
    let writes_before = device.writes;
    let flushes_before = device.flushes;

    let error = recover_journal_and_checkpoint_checked(&mut device, superblock).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains(&format!("allocated block {orphan} has no inode owner")));
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        0
    );
    assert!(!load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn pathname_metadata_uses_checked_recovery_for_valid_committed_wal() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    publish_root_inode(&mut device, superblock);

    let metadata = metadata_at_path(&mut device, &superblock, "/").unwrap();

    assert_eq!(metadata.inode_id, 1);
    assert_eq!(metadata.kind, InodeKind::Directory);
    assert_eq!(metadata.logical_blocks, 0);
    assert_eq!(metadata.namespace_references, 0);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn pathname_metadata_rejects_semantic_corruption_without_replay() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let orphan = publish_orphan_allocation(&mut device, superblock);
    let writes_before = device.writes;
    let flushes_before = device.flushes;

    let error = metadata_at_path(&mut device, &superblock, "/").unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains(&format!("allocated block {orphan} has no inode owner")));
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        0
    );
    assert!(!load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}
