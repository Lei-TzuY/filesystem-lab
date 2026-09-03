use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::format_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};

#[derive(Debug)]
struct MemoryDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
    writes: Vec<u64>,
    flushes: usize,
}

impl MemoryDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0_u8; BLOCK_SIZE]; blocks],
            writes: Vec::new(),
            flushes: 0,
        }
    }
}

impl BlockDevice for MemoryDevice {
    fn block_count(&self) -> u64 {
        u64::try_from(self.blocks.len()).unwrap()
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let source = self
            .blocks
            .get(usize::try_from(block).unwrap())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
        *buf = *source;
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let target = self
            .blocks
            .get_mut(usize::try_from(block).unwrap())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
        *target = *buf;
        self.writes.push(block);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

fn sample_inodes() -> Vec<PersistedInode> {
    vec![
        PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: vec![],
        },
        PersistedInode {
            id: 7,
            kind: InodeKind::File,
            blocks: vec![9, 11],
        },
    ]
}

#[test]
fn fresh_format_contains_empty_inode_table() {
    let mut device = MemoryDevice::new(32);
    let superblock = format_device(&mut device).unwrap();
    assert!(load_inode_table(&mut device, &superblock).unwrap().is_empty());
}

#[test]
fn inode_table_round_trip_flushes_and_writes_header_block_last() {
    let mut device = MemoryDevice::new(32);
    let superblock = format_device(&mut device).unwrap();
    device.writes.clear();
    let flushes_before = device.flushes;

    let inodes = sample_inodes();
    store_inode_table(&mut device, &superblock, &inodes).unwrap();

    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
    assert_eq!(device.flushes, flushes_before + 1);
    assert_eq!(device.writes, vec![superblock.inode_start + 1, superblock.inode_start]);
}

#[test]
fn duplicate_inode_ids_are_rejected_before_write() {
    let mut device = MemoryDevice::new(32);
    let superblock = format_device(&mut device).unwrap();
    device.writes.clear();
    let duplicate = vec![
        PersistedInode {
            id: 3,
            kind: InodeKind::File,
            blocks: vec![],
        },
        PersistedInode {
            id: 3,
            kind: InodeKind::Directory,
            blocks: vec![],
        },
    ];

    let error = store_inode_table(&mut device, &superblock, &duplicate).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(device.writes.is_empty());
}

#[test]
fn corruption_and_stale_padding_are_detected() {
    let mut device = MemoryDevice::new(32);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(&mut device, &superblock, &sample_inodes()).unwrap();

    device.blocks[usize::try_from(superblock.inode_start).unwrap()][40] ^= 0x80;
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    store_inode_table(&mut device, &superblock, &[]).unwrap();
    device.blocks[usize::try_from(superblock.inode_start).unwrap()][BLOCK_SIZE - 1] = 1;
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
