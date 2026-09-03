use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{format_device, Superblock};

#[derive(Debug)]
struct MemoryBlockDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
    flushes: usize,
}

impl MemoryBlockDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0; BLOCK_SIZE]; blocks],
            flushes: 0,
        }
    }
}

impl BlockDevice for MemoryBlockDevice {
    fn block_count(&self) -> u64 {
        u64::try_from(self.blocks.len()).expect("test device length fits u64")
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block index overflow"))?;
        let source = self
            .blocks
            .get(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "block out of range"))?;
        buf.copy_from_slice(source);
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block index overflow"))?;
        let target = self
            .blocks
            .get_mut(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "block out of range"))?;
        target.copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry {
        parent,
        target,
        name: name.to_owned(),
    }
}

#[test]
fn fresh_format_contains_empty_directory_table() {
    let mut device = MemoryBlockDevice::new(32);
    let superblock = format_device(&mut device).unwrap();
    assert!(load_directory_table(&mut device, &superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn directory_table_round_trip_and_flush() {
    let mut device = MemoryBlockDevice::new(32);
    let superblock = format_device(&mut device).unwrap();
    let entries = vec![entry(2, 3, "alpha"), entry(2, 4, "beta")];
    let before = device.flushes;

    store_directory_table(&mut device, &superblock, &entries).unwrap();

    assert_eq!(device.flushes, before + 1);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        entries
    );
}

#[test]
fn duplicate_parent_name_is_rejected_even_for_different_targets() {
    let mut device = MemoryBlockDevice::new(32);
    let superblock = format_device(&mut device).unwrap();
    let entries = vec![entry(2, 3, "same"), entry(2, 4, "same")];

    assert_eq!(
        store_directory_table(&mut device, &superblock, &entries)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn corruption_and_stale_padding_are_detected() {
    let mut device = MemoryBlockDevice::new(32);
    let superblock = format_device(&mut device).unwrap();
    store_directory_table(&mut device, &superblock, &[entry(2, 3, "alpha")]).unwrap();

    let first = usize::try_from(superblock.directory_start).unwrap();
    device.blocks[first][40] ^= 0x80;
    assert_eq!(
        load_directory_table(&mut device, &superblock)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );

    store_directory_table(&mut device, &superblock, &[entry(2, 3, "alpha")]).unwrap();
    device.blocks[first][BLOCK_SIZE - 1] = 1;
    assert_eq!(
        load_directory_table(&mut device, &superblock)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn geometry_mismatch_is_rejected() {
    let mut device = MemoryBlockDevice::new(32);
    let superblock = Superblock::new(31).unwrap();
    assert_eq!(
        load_directory_table(&mut device, &superblock)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}
