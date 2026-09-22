use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal::JournalLog;
use filesystem_lab::journal_region::store_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::recovery_projection::check_device_after_recovery_projection;

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

fn metadata_image(
    device: &mut MemoryDevice,
    superblock: &Superblock,
    block: u64,
) -> [u8; BLOCK_SIZE] {
    let mut data = [0; BLOCK_SIZE];
    device.read_block(block, &mut data).unwrap();
    assert!(
        superblock.allocation_range().contains(&block)
            || superblock.inode_range().contains(&block)
            || superblock.directory_range().contains(&block)
    );
    data
}

#[test]
fn projects_a_committed_root_inode_without_mutating_home_state() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
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
        metadata_image(&mut desired, &superblock, superblock.inode_start),
    )
    .unwrap();
    log.commit(txid).unwrap();
    store_journal_image(&mut device, superblock, log.entries()).unwrap();

    let raw = check_device(&mut device).unwrap();
    assert_eq!(raw.allocated_blocks, 0);
    assert_eq!(raw.inode_records, 0);
    assert_eq!(raw.directory_entries, 0);

    let writes_before = device.writes;
    let flushes_before = device.flushes;
    let projected = check_device_after_recovery_projection(&mut device).unwrap();

    assert_eq!(
        projected.recovery,
        RecoveryReport {
            committed_transactions: 1,
            home_writes: 1,
        }
    );
    assert_eq!(projected.fsck.allocated_blocks, 0);
    assert_eq!(projected.fsck.inode_records, 1);
    assert_eq!(projected.fsck.directory_entries, 0);
    assert_eq!(projected.fsck.referenced_blocks, 0);
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
    assert!(load_inode_table(&mut device, &superblock)
        .unwrap()
        .is_empty());
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        0
    );
}

#[test]
fn rejects_a_structurally_valid_commit_whose_projection_leaks_allocation() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut desired = device.clone();

    let mut allocator = load_allocator(&mut desired, &superblock).unwrap();
    let orphan = allocator.allocate().unwrap();
    store_allocator(&mut desired, &superblock, &allocator).unwrap();

    let mut log = JournalLog::new();
    let txid = log.begin().unwrap();
    log.write(
        txid,
        superblock.allocation_start,
        metadata_image(&mut desired, &superblock, superblock.allocation_start),
    )
    .unwrap();
    log.commit(txid).unwrap();
    store_journal_image(&mut device, superblock, log.entries()).unwrap();

    check_device(&mut device).unwrap();
    let writes_before = device.writes;
    let flushes_before = device.flushes;

    let error = check_device_after_recovery_projection(&mut device).unwrap_err();

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
}

#[test]
fn ignores_an_uncommitted_tail_exactly_like_recovery() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut desired = device.clone();

    let mut allocator = load_allocator(&mut desired, &superblock).unwrap();
    allocator.allocate().unwrap();
    store_allocator(&mut desired, &superblock, &allocator).unwrap();

    let mut log = JournalLog::new();
    let txid = log.begin().unwrap();
    log.write(
        txid,
        superblock.allocation_start,
        metadata_image(&mut desired, &superblock, superblock.allocation_start),
    )
    .unwrap();
    store_journal_image(&mut device, superblock, log.entries()).unwrap();

    let writes_before = device.writes;
    let flushes_before = device.flushes;
    let projected = check_device_after_recovery_projection(&mut device).unwrap();

    assert_eq!(projected.recovery, RecoveryReport::default());
    assert_eq!(projected.fsck.allocated_blocks, 0);
    assert_eq!(projected.fsck.inode_records, 0);
    assert_eq!(projected.fsck.pending_transaction, Some(txid));
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
}
