use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::{DEFAULT_DIRECTORY_BLOCKS, DEFAULT_INODE_BLOCKS};
use filesystem_lab::format_capacity::format_device_for_blockless_inode_capacity;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};

struct MemoryBlockDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
}

impl MemoryBlockDevice {
    fn new(blocks: usize) -> Self {
        Self { blocks: vec![[0; BLOCK_SIZE]; blocks] }
    }
}

impl BlockDevice for MemoryBlockDevice {
    fn block_count(&self) -> u64 { self.blocks.len() as u64 }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        buf.copy_from_slice(&self.blocks[block as usize]);
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        self.blocks[block as usize].copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

#[test]
fn formatter_reserves_enough_space_for_requested_blockless_inode_capacity() {
    let mut device = MemoryBlockDevice::new(256);
    let superblock = format_device_for_blockless_inode_capacity(&mut device, 8, 400).unwrap();

    assert!(superblock.inode_blocks > DEFAULT_INODE_BLOCKS);
    assert_eq!(superblock.directory_blocks, DEFAULT_DIRECTORY_BLOCKS);

    let inodes: Vec<_> = (1..=400)
        .map(|id| PersistedInode { id, kind: InodeKind::File, blocks: Vec::new() })
        .collect();
    store_inode_table(&mut device, &superblock, &inodes).unwrap();
    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
}

#[test]
fn zero_inode_capacity_is_rejected_before_formatting() {
    let mut device = MemoryBlockDevice::new(64);
    assert!(format_device_for_blockless_inode_capacity(&mut device, 8, 0).is_err());
    assert_eq!(device.blocks[0], [0; BLOCK_SIZE]);
}
