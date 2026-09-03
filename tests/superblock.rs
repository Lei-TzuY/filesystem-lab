use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::{
    format_device, read_superblock, required_allocation_blocks, Superblock,
    DEFAULT_DIRECTORY_BLOCKS, DEFAULT_INODE_BLOCKS, DEFAULT_JOURNAL_BLOCKS, FORMAT_VERSION,
    SUPERBLOCK_MAGIC,
};

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
    let superblock = Superblock::new(128).expect("valid superblock");
    let encoded = superblock.encode();
    assert_eq!(&encoded[0..8], &SUPERBLOCK_MAGIC);
    assert_eq!(
        u32::from_le_bytes(encoded[8..12].try_into().unwrap()),
        FORMAT_VERSION
    );
    assert_eq!(Superblock::decode(&encoded).unwrap(), superblock);
    assert_eq!(superblock.journal_range(), 1..1 + DEFAULT_JOURNAL_BLOCKS);
    assert_eq!(superblock.allocation_range(), 3..4);
    assert_eq!(superblock.inode_range(), 4..4 + DEFAULT_INODE_BLOCKS);
    assert_eq!(superblock.directory_range(), 6..6 + DEFAULT_DIRECTORY_BLOCKS);
    assert_eq!(superblock.reserved_blocks(), 8);
}

#[test]
fn explicit_journal_reservation_is_encoded() {
    let superblock = Superblock::with_journal_blocks(64, 7).unwrap();
    assert_eq!(superblock.journal_range(), 1..8);
    assert_eq!(superblock.allocation_range(), 8..9);
    assert_eq!(superblock.inode_range(), 9..11);
    assert_eq!(superblock.directory_range(), 11..13);
    assert_eq!(superblock.reserved_blocks(), 13);
    assert_eq!(Superblock::decode(&superblock.encode()).unwrap(), superblock);
}

#[test]
fn allocation_reservation_scales_with_device_size() {
    let total_blocks = 32_769_u64;
    assert_eq!(required_allocation_blocks(total_blocks).unwrap(), 2);
    let superblock = Superblock::new(total_blocks).unwrap();
    assert_eq!(superblock.allocation_range(), 3..5);
    assert_eq!(superblock.inode_range(), 5..7);
    assert_eq!(superblock.directory_range(), 7..9);
    assert_eq!(superblock.reserved_blocks(), 9);
}

#[test]
fn invalid_metadata_reservations_are_rejected() {
    assert_eq!(
        Superblock::with_journal_blocks(16, 0).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        Superblock::with_metadata_blocks(16, 2, 0)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        Superblock::with_all_metadata_blocks(16, 2, 2, 0)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        Superblock::with_journal_blocks(4, 4).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        Superblock::with_journal_blocks(u64::MAX, u64::MAX)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn format_persists_metadata_prefix_and_flushes() {
    let mut device = MemoryBlockDevice::new(16);
    let written = format_device(&mut device).unwrap();

    assert_eq!(written.total_blocks, 16);
    assert_eq!(written.journal_range(), 1..1 + DEFAULT_JOURNAL_BLOCKS);
    assert_eq!(written.allocation_range(), 3..4);
    assert_eq!(written.inode_range(), 4..6);
    assert_eq!(written.directory_range(), 6..8);
    assert_ne!(&device.blocks[3][0..8], &[0_u8; 8]);
    assert_ne!(&device.blocks[4][0..8], &[0_u8; 8]);
    assert_ne!(&device.blocks[6][0..8], &[0_u8; 8]);
    assert_eq!(device.flushes, 1);
    assert_eq!(read_superblock(&mut device).unwrap(), written);
}

#[test]
fn decode_rejects_bad_magic_version_metadata_layout_and_reserved_bytes() {
    let valid = Superblock::new(10).unwrap().encode();

    let mut bad_magic = valid;
    bad_magic[0] ^= 0xff;
    assert_eq!(Superblock::decode(&bad_magic).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut bad_version = valid;
    bad_version[8..12].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
    assert_eq!(Superblock::decode(&bad_version).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut bad_journal_start = valid;
    bad_journal_start[24..32].copy_from_slice(&2_u64.to_le_bytes());
    assert_eq!(Superblock::decode(&bad_journal_start).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut zero_journal = valid;
    zero_journal[32..40].copy_from_slice(&0_u64.to_le_bytes());
    assert_eq!(Superblock::decode(&zero_journal).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut bad_allocation_start = valid;
    bad_allocation_start[40..48].copy_from_slice(&4_u64.to_le_bytes());
    assert_eq!(Superblock::decode(&bad_allocation_start).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut bad_allocation_blocks = valid;
    bad_allocation_blocks[48..56].copy_from_slice(&2_u64.to_le_bytes());
    assert_eq!(Superblock::decode(&bad_allocation_blocks).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut bad_inode_start = valid;
    bad_inode_start[56..64].copy_from_slice(&5_u64.to_le_bytes());
    assert_eq!(Superblock::decode(&bad_inode_start).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut zero_inode_blocks = valid;
    zero_inode_blocks[64..72].copy_from_slice(&0_u64.to_le_bytes());
    assert_eq!(Superblock::decode(&zero_inode_blocks).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut bad_directory_start = valid;
    bad_directory_start[72..80].copy_from_slice(&7_u64.to_le_bytes());
    assert_eq!(Superblock::decode(&bad_directory_start).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut zero_directory_blocks = valid;
    zero_directory_blocks[80..88].copy_from_slice(&0_u64.to_le_bytes());
    assert_eq!(Superblock::decode(&zero_directory_blocks).unwrap_err().kind(), io::ErrorKind::InvalidData);

    let mut bad_reserved = valid;
    bad_reserved[88] = 1;
    assert_eq!(Superblock::decode(&bad_reserved).unwrap_err().kind(), io::ErrorKind::InvalidData);
}

#[test]
fn read_rejects_device_size_mismatch() {
    let mut device = MemoryBlockDevice::new(10);
    let encoded = Superblock::new(11).unwrap().encode();
    device.write_block(0, &encoded).unwrap();

    assert_eq!(
        read_superblock(&mut device).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn device_must_fit_all_durable_metadata() {
    let mut empty = MemoryBlockDevice::new(0);
    assert_eq!(format_device(&mut empty).unwrap_err().kind(), io::ErrorKind::InvalidInput);

    let mut superblock_only = MemoryBlockDevice::new(1);
    assert_eq!(
        format_device(&mut superblock_only).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );

    let mut missing_directory_table = MemoryBlockDevice::new(7);
    assert_eq!(
        format_device(&mut missing_directory_table).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}
