use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_table::load_directory_table;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal::JournalLog;
use filesystem_lab::journal_region::{load_journal_image, store_journal_image};
use filesystem_lab::path_create::create_empty_file_at_path_journaled;
use filesystem_lab::path_directory::list_directory_at_path;

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

fn setup_root_with_bad_committed_wal() -> (MemoryDevice, Superblock, u64) {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        }],
    )
    .unwrap();

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
    store_journal_image(&mut device, superblock, log.entries()).unwrap();

    (device, superblock, orphan)
}

fn assert_bad_wal_remains_unapplied(
    device: &mut MemoryDevice,
    superblock: Superblock,
    orphan: u64,
    writes_before: usize,
    flushes_before: usize,
) {
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
    assert_eq!(
        load_allocator(device, &superblock)
            .unwrap()
            .allocated_blocks(),
        0
    );
    assert_eq!(
        load_inode_table(device, &superblock).unwrap(),
        vec![PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        }]
    );
    assert!(load_directory_table(device, &superblock)
        .unwrap()
        .is_empty());
    assert!(!load_journal_image(device, superblock).unwrap().is_empty());
    assert!(orphan >= superblock.reserved_blocks());
}

#[test]
fn namespace_create_rejects_semantic_bad_wal_before_home_replay() {
    let (mut device, superblock, orphan) = setup_root_with_bad_committed_wal();
    let writes_before = device.writes;
    let flushes_before = device.flushes;

    let error = create_empty_file_at_path_journaled(&mut device, &superblock, "/new").unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains(&format!("allocated block {orphan} has no inode owner")));
    assert_bad_wal_remains_unapplied(
        &mut device,
        superblock,
        orphan,
        writes_before,
        flushes_before,
    );
}

#[test]
fn namespace_listing_rejects_semantic_bad_wal_before_home_replay() {
    let (mut device, superblock, orphan) = setup_root_with_bad_committed_wal();
    let writes_before = device.writes;
    let flushes_before = device.flushes;

    let error = list_directory_at_path(&mut device, &superblock, "/").unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains(&format!("allocated block {orphan} has no inode owner")));
    assert_bad_wal_remains_unapplied(
        &mut device,
        superblock,
        orphan,
        writes_before,
        flushes_before,
    );
}
