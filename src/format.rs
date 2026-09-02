use std::io;
use std::ops::Range;

use crate::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};

pub const SUPERBLOCK_BLOCK: u64 = 0;
pub const SUPERBLOCK_MAGIC: [u8; 8] = *b"FSLABFS\0";
pub const FORMAT_VERSION: u32 = 2;
pub const FORMAT_BLOCK_SIZE: u32 = 4096;
pub const DEFAULT_JOURNAL_BLOCKS: u64 = 1;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 8;
const BLOCK_SIZE_OFFSET: usize = 12;
const TOTAL_BLOCKS_OFFSET: usize = 16;
const JOURNAL_START_OFFSET: usize = 24;
const JOURNAL_BLOCKS_OFFSET: usize = 32;
const HEADER_LEN: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
    pub total_blocks: u64,
    pub journal_start: u64,
    pub journal_blocks: u64,
}

impl Superblock {
    /// Creates a version-2 superblock using the default durable journal reservation.
    ///
    /// # Errors
    ///
    /// Returns an error if the device cannot contain the superblock and journal reservation.
    pub fn new(total_blocks: u64) -> io::Result<Self> {
        Self::with_journal_blocks(total_blocks, DEFAULT_JOURNAL_BLOCKS)
    }

    /// Creates a version-2 superblock with an explicit contiguous journal reservation.
    ///
    /// The journal always begins immediately after block zero. This keeps all currently defined
    /// metadata in one reserved prefix so the in-memory allocator can exclude it deterministically.
    ///
    /// # Errors
    ///
    /// Returns an error if `journal_blocks` is zero, arithmetic overflows, or the journal would not
    /// fit on the device.
    pub fn with_journal_blocks(total_blocks: u64, journal_blocks: u64) -> io::Result<Self> {
        if journal_blocks == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "filesystem journal must reserve at least one block",
            ));
        }

        let journal_start = SUPERBLOCK_BLOCK + 1;
        let journal_end = journal_start.checked_add(journal_blocks).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "journal block range overflow")
        })?;
        if journal_end > total_blocks {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "filesystem device is too small for the journal reservation",
            ));
        }

        Ok(Self {
            total_blocks,
            journal_start,
            journal_blocks,
        })
    }

    #[must_use]
    pub fn journal_range(self) -> Range<u64> {
        self.journal_start..self.journal_start + self.journal_blocks
    }

    #[must_use]
    pub fn reserved_blocks(self) -> u64 {
        self.journal_start + self.journal_blocks
    }

    #[must_use]
    pub fn encode(self) -> [u8; BLOCK_SIZE] {
        let mut block = [0_u8; BLOCK_SIZE];
        block[MAGIC_OFFSET..MAGIC_OFFSET + SUPERBLOCK_MAGIC.len()]
            .copy_from_slice(&SUPERBLOCK_MAGIC);
        block[VERSION_OFFSET..VERSION_OFFSET + 4].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
        block[BLOCK_SIZE_OFFSET..BLOCK_SIZE_OFFSET + 4]
            .copy_from_slice(&FORMAT_BLOCK_SIZE.to_le_bytes());
        block[TOTAL_BLOCKS_OFFSET..TOTAL_BLOCKS_OFFSET + 8]
            .copy_from_slice(&self.total_blocks.to_le_bytes());
        block[JOURNAL_START_OFFSET..JOURNAL_START_OFFSET + 8]
            .copy_from_slice(&self.journal_start.to_le_bytes());
        block[JOURNAL_BLOCKS_OFFSET..JOURNAL_BLOCKS_OFFSET + 8]
            .copy_from_slice(&self.journal_blocks.to_le_bytes());
        block
    }

    /// Decodes and validates a version-2 superblock block.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` when the magic, format version, logical block size, journal layout,
    /// reserved bytes, or total block count is invalid.
    pub fn decode(block: &[u8; BLOCK_SIZE]) -> io::Result<Self> {
        if block[MAGIC_OFFSET..MAGIC_OFFSET + SUPERBLOCK_MAGIC.len()] != SUPERBLOCK_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid superblock magic",
            ));
        }

        let version = read_u32_le(block, VERSION_OFFSET);
        if version != FORMAT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported filesystem format version {version}"),
            ));
        }

        let block_size = read_u32_le(block, BLOCK_SIZE_OFFSET);
        if block_size != FORMAT_BLOCK_SIZE || u64::from(block_size) != BLOCK_SIZE_U64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported logical block size",
            ));
        }

        let total_blocks = read_u64_le(block, TOTAL_BLOCKS_OFFSET);
        let journal_start = read_u64_le(block, JOURNAL_START_OFFSET);
        let journal_blocks = read_u64_le(block, JOURNAL_BLOCKS_OFFSET);
        if journal_start != SUPERBLOCK_BLOCK + 1 || journal_blocks == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid journal reservation",
            ));
        }
        let journal_end = journal_start.checked_add(journal_blocks).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "journal block range overflow")
        })?;
        if journal_end > total_blocks {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "journal reservation exceeds filesystem size",
            ));
        }

        if block[HEADER_LEN..].iter().any(|byte| *byte != 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "superblock reserved bytes are non-zero",
            ));
        }

        Ok(Self {
            total_blocks,
            journal_start,
            journal_blocks,
        })
    }
}

fn read_u32_le(block: &[u8; BLOCK_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes([
        block[offset],
        block[offset + 1],
        block[offset + 2],
        block[offset + 3],
    ])
}

fn read_u64_le(block: &[u8; BLOCK_SIZE], offset: usize) -> u64 {
    u64::from_le_bytes([
        block[offset],
        block[offset + 1],
        block[offset + 2],
        block[offset + 3],
        block[offset + 4],
        block[offset + 5],
        block[offset + 6],
        block[offset + 7],
    ])
}

/// Writes a freshly encoded superblock to block zero and flushes it through the device durability
/// boundary.
///
/// # Errors
///
/// Returns an error if the device cannot hold the version-2 metadata reservation, writing fails, or
/// flushing fails.
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
