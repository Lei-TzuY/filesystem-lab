use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::{format_device, read_superblock, Superblock, FORMAT_VERSION, SUPERBLOCK_MAGIC};

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

#[test]
fn superblock_round_trip_is_deterministic() {
    let encoded = Superblock::new(128).expect("valid superblock").encode();
    assert_eq!(&encoded[0..8], &SUPERBLOCK_MAGIC);
    assert_eq!(u32::from_le_bytes(encoded[8..12].try_into().unwrap()), FORMAT_VERSION);
    assert_eq!(Superblock::decode(&encoded).unwrap().total_blocks, 128);
}

#[test]
fn format_persists_block_zero_and_flushes() {
    let mut device = MemoryBlockDevice::new(16);
    let written = format_device(&mut device).unwrap();

    assert_eq!(written.total_blocks, 16);
    assert_eq!(device.flushes, 1);
    assert_eq!(read_superblock(&mut device).unwrap(), written);
}

#[test]
fn decode_rejects_bad_magic_version_and_reserved_bytes() {
    let valid = Superblock::new(8).unwrap().encode();

    let mut bad_magic = valid;
    bad_magic[0] ^= 0xff;
    assert_eq!(Superblock::decode(&bad_magic).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut bad_version = valid;
    bad_version[8..12].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
    assert_eq!(Superblock::decode(&bad_version).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut bad_reserved = valid;
    bad_reserved[24] = 1;
    assert_eq!(Superblock::decode(&bad_reserved).unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn read_rejects_device_size_mismatch() {
    let mut device = MemoryBlockDevice::new(8);
    let encoded = Superblock::new(9).unwrap().encode();
    device.write_block(0, &encoded).unwrap();

    assert_eq!(read_superblock(&mut device).unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn empty_device_cannot_be_formatted() {
    let mut device = MemoryBlockDevice::new(0);
    assert_eq!(format_device(&mut device).unwrap_err().kind(), io::ErrorKind::InvalidInput);
}
