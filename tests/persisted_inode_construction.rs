use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::format_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};

#[derive(Debug)]
struct MemoryDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
}

impl MemoryDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0; BLOCK_SIZE]; blocks],
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
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn constructed_inode_round_trips_through_durable_table() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let root = PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap();
    let file = PersistedInode::new(
        2,
        InodeKind::File,
        vec![
            superblock.reserved_blocks(),
            superblock.reserved_blocks() + 1,
        ],
    )
    .unwrap();

    store_inode_table(&mut device, &superblock, &[root.clone(), file.clone()]).unwrap();

    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        vec![root, file]
    );
}

#[test]
fn constructor_rejects_invalid_inode_before_table_publication() {
    let zero = PersistedInode::new(0, InodeKind::File, Vec::new()).unwrap_err();
    assert_eq!(zero.kind(), io::ErrorKind::InvalidInput);

    let duplicate = PersistedInode::new(2, InodeKind::File, vec![9, 9]).unwrap_err();
    assert_eq!(duplicate.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn inode_table_remains_defensive_against_legacy_direct_literals() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let invalid = PersistedInode {
        id: 2,
        kind: InodeKind::File,
        blocks: vec![superblock.reserved_blocks(), superblock.reserved_blocks()],
    
        byte_len: 0,
    };

    let error = store_inode_table(&mut device, &superblock, &[invalid]).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
