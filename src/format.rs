use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};

pub const SUPERBLOCK_BLOCK: u64 = 0;
pub const SUPERBLOCK_MAGIC: [u8; 8] = *b"FSLABFS\0";
pub const FORMAT_VERSION: u32 = 1;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 8;
const BLOCK_SIZE_OFFSET: usize = 12;
const TOTAL_BLOCKS_OFFSET: usize = 16;
const HEADER_LEN: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
    pub total_blocks: u64,
}

impl Superblock {
    /// Creates a version-1 superblock for a device of `total_blocks` blocks.
    ///
    /// # Errors
    ///
    /// Returns an error if the device has no room for the superblock itself.
    pub fn new(total_blocks: u64) -> io::Result<Self> {
        if total_blocks == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "filesystem requires at least one block",
            ));
        }
        Ok(Self { total_blocks })
    }

    #[must_use]
    pub fn encode(self) -> [u8; BLOCK_SIZE] {
        let mut block = [0_u8; BLOCK_SIZE];
        block[MAGIC_OFFSET..MAGIC_OFFSET + SUPERBLOCK_MAGIC.len()]
            .copy_from_slice(&SUPERBLOCK_MAGIC);
        block[VERSION_OFFSET..VERSION_OFFSET + 4].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        block[BLOCK_SIZE_OFFSET..BLOCK_SIZE_OFFSET + 4]
            .copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        block[TOTAL_BLOCKS_OFFSET..TOTAL_BLOCKS_OFFSET + 8]
            .copy_from_slice(&self.total_blocks.to_le_bytes());
        block
    }

    /// Decodes and validates a version-1 superblock block.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when the magic, format version, logical block size, reserved bytes, or
    /// total block count is invalid.
    pub fn decode(block: &[u8; BLOCK_SIZE]) -> io::Result<Self> {
        if block[MAGIC_OFFSET..MAGIC_OFFSET + SUPERBLOCK_MAGIC.len()] != SUPERBLOCK_MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid superblock magic"));
        }

        let version = u32::from_le_bytes(block[VERSION_OFFSET..VERSION_OFFSET + 4].try_into().expect("fixed slice"));
        if version != FORMAT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported filesystem format version {version}"),
            ));
        }

        let block_size = u32::from_le_bytes(block[BLOCK_SIZE_OFFSET..BLOCK_SIZE_OFFSET + 4].try_into().expect("fixed slice"));
        if u64::from(block_size) != BLOCK_SIZE_U64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "unsupported logical block size"));
        }

        let total_blocks = u64::from_le_bytes(block[TOTAL_BLOCKS_OFFSET..TOTAL_BLOCKS_OFFSET + 8].try_into().expect("fixed slice"));
        if total_blocks == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "superblock declares zero blocks"));
        }

        if block[HEADER_LEN..].iter().any(|byte| *byte != 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "superblock reserved bytes are non-zero",
            ));
        }

        Ok(Self { total_blocks })
    }
}

/// Writes a freshly encoded superblock to block zero and flushes it through the device durability
/// boundary.
///
/// # Errors
///
/// Returns an error if the device cannot hold a superblock, writing fails, or flushing fails.
pub fn format_device(device: &mut impl BlockDevice) -> io::Result<Superblock> {
    let superblock = Superblock::new(device.block_count())?;
    device.write_block(SUPERBLOCK_BLOCK, &superblock.encode())?;
    device.flush()?;
    Ok(superblock)
}

/// Reads and validates the superblock against the currently opened block device.
///
/// # Errors
///
/// Returns an error when block zero cannot be read, the encoded superblock is invalid, or its
/// recorded device size does not match the block device.
pub fn read_superblock(device: &mut impl BlockDevice) -> io::Result<Superblock> {
    let mut block = [0_u8; BLOCK_SIZE];
    device.read_block(SUPERBLOCK_BLOCK, &mut block)?;
    let superblock = Superblock::decode(&block)?;
    if superblock.total_blocks != device.block_count() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "superblock block count does not match device",
        ));
    }
    Ok(superblock)
}
