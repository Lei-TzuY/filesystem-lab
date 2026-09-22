use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_table::load_directory_table;
use filesystem_lab::format::read_superblock;
use filesystem_lab::format_geometry::{
    format_device_with_journal_blocks, format_device_with_metadata_blocks,
};
use filesystem_lab::inode_table::load_inode_table;
use filesystem_lab::journal_region::load_journal_image;

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

    fn poison_blocks(&mut self, start: usize, end: usize) {
        for block in &mut self.blocks[start..end] {
            block.fill(0xa5);
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

fn assert_clean_metadata(device: &mut MemoryDevice, superblock: filesystem_lab::format::Superblock) {
    assert_eq!(read_superblock(device).unwrap(), superblock);
    assert!(load_journal_image(device, superblock).unwrap().is_empty());
    assert_eq!(
        load_allocator(device, &superblock)
            .unwrap()
            .allocated_blocks(),
        0
    );
    assert!(load_inode_table(device, &superblock).unwrap().is_empty());
    assert!(load_directory_table(device, &superblock).unwrap().is_empty());
}

#[test]
fn custom_journal_geometry_replaces_stale_journal_bytes() {
    let mut device = MemoryDevice::new(64);
    let journal_blocks = 8_u64;
    device.poison_blocks(1, 1 + usize::try_from(journal_blocks).unwrap());

    let superblock = format_device_with_journal_blocks(&mut device, journal_blocks).unwrap();

    assert_eq!(superblock.journal_blocks, journal_blocks);
    assert_clean_metadata(&mut device, superblock);
}

#[test]
fn custom_metadata_geometry_replaces_stale_journal_bytes() {
    let mut device = MemoryDevice::new(64);
    let journal_blocks = 7_u64;
    device.poison_blocks(1, 1 + usize::try_from(journal_blocks).unwrap());

    let superblock =
        format_device_with_metadata_blocks(&mut device, journal_blocks, 3, 4).unwrap();

    assert_eq!(superblock.journal_blocks, journal_blocks);
    assert_eq!(superblock.inode_blocks, 3);
    assert_eq!(superblock.directory_blocks, 4);
    assert_clean_metadata(&mut device, superblock);
}
