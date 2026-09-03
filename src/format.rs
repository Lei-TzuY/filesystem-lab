use std::io;
use std::ops::Range;

use crate::allocation_disk::initialize_allocation_region;
use crate::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};
use crate::inode_table::initialize_inode_table_region;

pub const SUPERBLOCK_BLOCK: u64 = 0;
pub const SUPERBLOCK_MAGIC: [u8; 8] = *b"FSLABFS\0";
pub const FORMAT_VERSION: u32 = 4;
pub const FORMAT_BLOCK_SIZE: u32 = 4096;
pub const DEFAULT_JOURNAL_BLOCKS: u64 = 2;
pub const DEFAULT_INODE_BLOCKS: u64 = 2;
pub const ALLOCATION_IMAGE_HEADER_LEN: u64 = 32;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 8;
const BLOCK_SIZE_OFFSET: usize = 12;
const TOTAL_BLOCKS_OFFSET: usize = 16;
const JOURNAL_START_OFFSET: usize = 24;
const JOURNAL_BLOCKS_OFFSET: usize = 32;
const ALLOCATION_START_OFFSET: usize = 40;
const ALLOCATION_BLOCKS_OFFSET: usize = 48;
const INODE_START_OFFSET: usize = 56;
const INODE_BLOCKS_OFFSET: usize = 64;
const HEADER_LEN: usize = 72;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
    pub total_blocks: u64,
    pub journal_start: u64,
    pub journal_blocks: u64,
    pub allocation_start: u64,
    pub allocation_blocks: u64,
    pub inode_start: u64,
    pub inode_blocks: u64,
}

impl Superblock {
    /// Creates a version-4 superblock using the default journal and inode reservations.
    ///
    /// # Errors
    ///
    /// Returns an error if the device cannot contain all durable metadata reservations.
    pub fn new(total_blocks: u64) -> io::Result<Self> {
        Self::with_metadata_blocks(total_blocks, DEFAULT_JOURNAL_BLOCKS, DEFAULT_INODE_BLOCKS)
    }

    /// Creates a version-4 superblock with an explicit contiguous journal reservation.
    ///
    /// The default inode-table reservation is retained.
    ///
    /// # Errors
    ///
    /// Returns an error if metadata geometry is invalid or exceeds the device.
    pub fn with_journal_blocks(total_blocks: u64, journal_blocks: u64) -> io::Result<Self> {
        Self::with_metadata_blocks(total_blocks, journal_blocks, DEFAULT_INODE_BLOCKS)
    }

    /// Creates a version-4 superblock with explicit journal and inode-table reservations.
    ///
    /// Durable metadata occupies one deterministic prefix: superblock, journal, allocation image,
    /// then inode table. Allocation reservation length is derived from filesystem size.
    ///
    /// # Errors
    ///
    /// Returns an error for zero-sized reservations, arithmetic overflow, or insufficient space.
    pub fn with_metadata_blocks(
        total_blocks: u64,
        journal_blocks: u64,
        inode_blocks: u64,
    ) -> io::Result<Self> {
        if journal_blocks == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "filesystem journal must reserve at least one block",
            ));
        }
        if inode_blocks == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "filesystem inode table must reserve at least one block",
            ));
        }

        let journal_start = SUPERBLOCK_BLOCK + 1;
        let allocation_start = journal_start.checked_add(journal_blocks).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "journal block range overflow")
        })?;
        let allocation_blocks = required_allocation_blocks(total_blocks)?;
        let inode_start = allocation_start
            .checked_add(allocation_blocks)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "allocation metadata block range overflow",
                )
            })?;
        let metadata_end = inode_start.checked_add(inode_blocks).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "inode table block range overflow",
            )
        })?;
        if metadata_end > total_blocks {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "filesystem device is too small for durable metadata reservations",
            ));
        }

        Ok(Self {
            total_blocks,
            journal_start,
            journal_blocks,
            allocation_start,
            allocation_blocks,
            inode_start,
            inode_blocks,
        })
    }

    #[must_use]
    pub fn journal_range(self) -> Range<u64> {
        self.journal_start..self.journal_start + self.journal_blocks
    }

    #[must_use]
    pub fn allocation_range(self) -> Range<u64> {
        self.allocation_start..self.allocation_start + self.allocation_blocks
    }

    #[must_use]
    pub fn inode_range(self) -> Range<u64> {
        self.inode_start..self.inode_start + self.inode_blocks
    }

    #[must_use]
    pub fn reserved_blocks(self) -> u64 {
        self.inode_start + self.inode_blocks
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
        block[ALLOCATION_START_OFFSET..ALLOCATION_START_OFFSET + 8]
            .copy_from_slice(&self.allocation_start.to_le_bytes());
        block[ALLOCATION_BLOCKS_OFFSET..ALLOCATION_BLOCKS_OFFSET + 8]
            .copy_from_slice(&self.allocation_blocks.to_le_bytes());
        block[INODE_START_OFFSET..INODE_START_OFFSET + 8]
            .copy_from_slice(&self.inode_start.to_le_bytes());
        block[INODE_BLOCKS_OFFSET..INODE_BLOCKS_OFFSET + 8]
            .copy_from_slice(&self.inode_blocks.to_le_bytes());
        block
    }

    /// Decodes and validates a version-4 superblock block.
    ///
    /// # Errors
    ///
    /// Returns `InvalidData` for unsupported versions, malformed geometry, or non-zero reserved
    /// bytes. Version-3 images are intentionally not reinterpreted as version 4.
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
        let allocation_start = read_u64_le(block, ALLOCATION_START_OFFSET);
        let allocation_blocks = read_u64_le(block, ALLOCATION_BLOCKS_OFFSET);
        let inode_start = read_u64_le(block, INODE_START_OFFSET);
        let inode_blocks = read_u64_le(block, INODE_BLOCKS_OFFSET);
        if journal_start != SUPERBLOCK_BLOCK + 1 || journal_blocks == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid journal reservation",
            ));
        }
        if inode_blocks == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid inode table reservation",
            ));
        }

        let expected_allocation_start =
            journal_start.checked_add(journal_blocks).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "journal block range overflow")
            })?;
        if allocation_start != expected_allocation_start {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "allocation metadata does not immediately follow journal",
            ));
        }
        let expected_allocation_blocks = required_allocation_blocks(total_blocks)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        if allocation_blocks != expected_allocation_blocks {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "allocation metadata reservation has invalid length",
            ));
        }
        let expected_inode_start =
            allocation_start
                .checked_add(allocation_blocks)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "allocation metadata block range overflow",
                    )
                })?;
        if inode_start != expected_inode_start {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "inode table does not immediately follow allocation metadata",
            ));
        }
        let metadata_end = inode_start.checked_add(inode_blocks).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "inode table block range overflow",
            )
        })?;
        if metadata_end > total_blocks {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "durable metadata reservation exceeds filesystem size",
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
            allocation_start,
            allocation_blocks,
            inode_start,
            inode_blocks,
        })
    }
}

/// Returns the number of bytes needed to represent one allocation bit per filesystem block.
///
/// # Errors
///
/// Returns an error if rounding arithmetic overflows.
pub fn allocation_bitmap_bytes(total_blocks: u64) -> io::Result<u64> {
    total_blocks
        .checked_add(7)
        .map(|rounded| rounded / 8)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "allocation bitmap size overflow",
            )
        })
}

/// Returns the number of 4 KiB blocks required by the version-1 allocation image.
///
/// # Errors
///
/// Returns an error if size arithmetic overflows.
pub fn required_allocation_blocks(total_blocks: u64) -> io::Result<u64> {
    let bytes = allocation_bitmap_bytes(total_blocks)?;
    let image_bytes = ALLOCATION_IMAGE_HEADER_LEN
        .checked_add(bytes)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "allocation image size overflow",
            )
        })?;
    image_bytes
        .checked_add(BLOCK_SIZE_U64 - 1)
        .map(|rounded| rounded / BLOCK_SIZE_U64)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "allocation image size overflow",
            )
        })
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

/// Writes a fresh format-v4 metadata prefix and flushes it through the durability boundary.
///
/// Allocation and inode metadata are initialized before the superblock is published, so a
/// successful superblock write never points at uninitialized durable metadata.
///
/// # Errors
///
/// Returns an error when the device is too small, metadata initialization fails, or I/O fails.
pub fn format_device(device: &mut impl BlockDevice) -> io::Result<Superblock> {
    let superblock = Superblock::new(device.block_count())?;
    initialize_allocation_region(device, &superblock)?;
    initialize_inode_table_region(device, &superblock)?;
    device.write_block(SUPERBLOCK_BLOCK, &superblock.encode())?;
    device.flush()?;
    Ok(superblock)
}

/// Reads and validates the superblock against the currently opened block device.
///
/// # Errors
///
/// Returns an error when block zero cannot be read, the superblock is invalid, or its device size
/// does not match the opened block device.
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
