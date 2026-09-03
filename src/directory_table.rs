use std::collections::BTreeSet;
use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::directory_codec::{
    decode_directory_entry, encode_directory_entry, PersistedDirectoryEntry,
    DIRECTORY_ENTRY_HEADER_LEN,
};
use crate::format::Superblock;

pub const DIRECTORY_TABLE_MAGIC: [u8; 8] = *b"FSLDIR\0\0";
pub const DIRECTORY_TABLE_VERSION: u32 = 1;
pub const DIRECTORY_TABLE_HEADER_LEN: usize = 32;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 8;
const FLAGS_OFFSET: usize = 12;
const PAYLOAD_BYTES_OFFSET: usize = 16;
const RECORD_COUNT_OFFSET: usize = 24;
const CHECKSUM_OFFSET: usize = 28;

/// Loads and validates the persistent directory table.
///
/// Entries are stored as a checksummed concatenation of self-delimiting `DNT1` records. A parent
/// may contain at most one entry with a given name. Mandatory zero padding prevents stale records
/// from being silently resurrected after a shorter snapshot rewrite.
///
/// # Errors
///
/// Returns `InvalidData` for malformed headers, checksum failure, non-zero padding, duplicate
/// `(parent, name)` keys, corrupt records, or inconsistent record counts. Device read failures
/// propagate.
pub fn load_directory_table(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
) -> io::Result<Vec<PersistedDirectoryEntry>> {
    validate_geometry(device, superblock)?;
    let image = read_region(device, superblock)?;
    decode_image(&image)
}

/// Stores a complete directory-table snapshot and crosses the device flush durability boundary.
///
/// Tail blocks are written before the header-bearing first block. The checksum plus mandatory zero
/// padding make a mixed old/new multi-block image detectable rather than silently accepted.
///
/// # Errors
///
/// Returns `InvalidInput` for duplicate `(parent, name)` keys, invalid records, or an image that
/// exceeds the reserved directory region. Device write/flush failures propagate.
pub fn store_directory_table(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    entries: &[PersistedDirectoryEntry],
) -> io::Result<()> {
    validate_geometry(device, superblock)?;
    let image = encode_image(superblock, entries)?;
    write_region(device, superblock, &image)?;
    device.flush()
}

pub(crate) fn initialize_directory_table_region(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
) -> io::Result<()> {
    validate_geometry(device, superblock)?;
    let image = encode_image(superblock, &[])?;
    write_region(device, superblock, &image)
}

fn validate_geometry(device: &impl BlockDevice, superblock: &Superblock) -> io::Result<()> {
    if device.block_count() != superblock.total_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory table device geometry does not match superblock",
        ));
    }
    if superblock.directory_blocks == 0
        || superblock.directory_range().end > superblock.total_blocks
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid directory table reservation",
        ));
    }
    Ok(())
}

fn region_len(superblock: &Superblock) -> io::Result<usize> {
    let blocks = usize::try_from(superblock.directory_blocks).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory table block count is too large",
        )
    })?;
    blocks.checked_mul(BLOCK_SIZE).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory table byte size overflow",
        )
    })
}

fn encode_image(
    superblock: &Superblock,
    entries: &[PersistedDirectoryEntry],
) -> io::Result<Vec<u8>> {
    let mut seen = BTreeSet::new();
    let mut payload = Vec::new();
    for entry in entries {
        if !seen.insert((entry.parent, entry.name.as_str())) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "directory table contains duplicate parent/name entry",
            ));
        }
        payload.extend_from_slice(&encode_directory_entry(entry)?);
    }

    let capacity = region_len(superblock)?;
    let used = DIRECTORY_TABLE_HEADER_LEN
        .checked_add(payload.len())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "directory image size overflow")
        })?;
    if used > capacity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory table image exceeds reserved region",
        ));
    }

    let mut image = vec![0_u8; capacity];
    image[MAGIC_OFFSET..MAGIC_OFFSET + DIRECTORY_TABLE_MAGIC.len()]
        .copy_from_slice(&DIRECTORY_TABLE_MAGIC);
    image[VERSION_OFFSET..VERSION_OFFSET + 4]
        .copy_from_slice(&DIRECTORY_TABLE_VERSION.to_le_bytes());
    image[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&0_u32.to_le_bytes());
    image[PAYLOAD_BYTES_OFFSET..PAYLOAD_BYTES_OFFSET + 8].copy_from_slice(
        &u64::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "directory payload too large"))?
            .to_le_bytes(),
    );
    image[RECORD_COUNT_OFFSET..RECORD_COUNT_OFFSET + 4].copy_from_slice(
        &u32::try_from(entries.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many directory records"))?
            .to_le_bytes(),
    );
    image[DIRECTORY_TABLE_HEADER_LEN..used].copy_from_slice(&payload);
    let checksum = crc32(&image[..used]);
    image[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
    Ok(image)
}

fn decode_image(image: &[u8]) -> io::Result<Vec<PersistedDirectoryEntry>> {
    if image.len() < DIRECTORY_TABLE_HEADER_LEN {
        return Err(invalid_data("directory table region is shorter than header"));
    }
    if image[MAGIC_OFFSET..MAGIC_OFFSET + DIRECTORY_TABLE_MAGIC.len()] != DIRECTORY_TABLE_MAGIC {
        return Err(invalid_data("invalid directory table magic"));
    }
    if read_u32(image, VERSION_OFFSET) != DIRECTORY_TABLE_VERSION {
        return Err(invalid_data("unsupported directory table version"));
    }
    if read_u32(image, FLAGS_OFFSET) != 0 {
        return Err(invalid_data("directory table flags are non-zero"));
    }

    let payload_len = usize::try_from(read_u64(image, PAYLOAD_BYTES_OFFSET))
        .map_err(|_| invalid_data("directory table payload exceeds usize"))?;
    let record_count = usize::try_from(read_u32(image, RECORD_COUNT_OFFSET))
        .map_err(|_| invalid_data("directory table record count exceeds usize"))?;
    let used = DIRECTORY_TABLE_HEADER_LEN
        .checked_add(payload_len)
        .ok_or_else(|| invalid_data("directory table used length overflow"))?;
    if used > image.len() {
        return Err(invalid_data("directory table payload exceeds reserved region"));
    }
    if image[used..].iter().any(|byte| *byte != 0) {
        return Err(invalid_data("directory table trailing padding is non-zero"));
    }

    let stored_checksum = read_u32(image, CHECKSUM_OFFSET);
    let mut checksummed = image[..used].to_vec();
    checksummed[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].fill(0);
    if crc32(&checksummed) != stored_checksum {
        return Err(invalid_data("directory table checksum mismatch"));
    }

    let payload = &image[DIRECTORY_TABLE_HEADER_LEN..used];
    let mut offset = 0_usize;
    let mut entries = Vec::with_capacity(record_count);
    let mut seen = BTreeSet::new();
    while offset < payload.len() {
        let remaining = &payload[offset..];
        if remaining.len() < DIRECTORY_ENTRY_HEADER_LEN {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "torn directory entry in table",
            ));
        }
        let total_len = usize::try_from(read_u32(remaining, 8))
            .map_err(|_| invalid_data("directory record length exceeds usize"))?;
        if total_len < DIRECTORY_ENTRY_HEADER_LEN {
            return Err(invalid_data(
                "directory record length is smaller than header",
            ));
        }
        let end = offset
            .checked_add(total_len)
            .ok_or_else(|| invalid_data("directory record offset overflow"))?;
        if end > payload.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "torn directory entry in table",
            ));
        }
        let entry = decode_directory_entry(&payload[offset..end])?;
        if !seen.insert((entry.parent, entry.name.clone())) {
            return Err(invalid_data(
                "directory table contains duplicate parent/name entry",
            ));
        }
        entries.push(entry);
        offset = end;
    }

    if entries.len() != record_count {
        return Err(invalid_data("directory table record count mismatch"));
    }
    Ok(entries)
}

fn read_region(device: &mut impl BlockDevice, superblock: &Superblock) -> io::Result<Vec<u8>> {
    let mut image = vec![0_u8; region_len(superblock)?];
    for (index, block) in superblock.directory_range().enumerate() {
        let start = index
            .checked_mul(BLOCK_SIZE)
            .ok_or_else(|| invalid_data("directory table offset overflow"))?;
        let end = start + BLOCK_SIZE;
        let target: &mut [u8; BLOCK_SIZE] = (&mut image[start..end])
            .try_into()
            .map_err(|_| invalid_data("directory table block slice mismatch"))?;
        device.read_block(block, target)?;
    }
    Ok(image)
}

fn write_region(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    image: &[u8],
) -> io::Result<()> {
    if image.len() != region_len(superblock)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory table image has invalid region length",
        ));
    }
    let blocks: Vec<u64> = superblock.directory_range().collect();
    for index in (1..blocks.len()).rev() {
        write_image_block(device, blocks[index], image, index)?;
    }
    if let Some(first) = blocks.first() {
        write_image_block(device, *first, image, 0)?;
    }
    Ok(())
}

fn write_image_block(
    device: &mut impl BlockDevice,
    block: u64,
    image: &[u8],
    index: usize,
) -> io::Result<()> {
    let start = index.checked_mul(BLOCK_SIZE).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory table offset overflow",
        )
    })?;
    let end = start + BLOCK_SIZE;
    let source: &[u8; BLOCK_SIZE] = image[start..end].try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory table block slice mismatch",
        )
    })?;
    device.write_block(block, source)
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for (index, byte) in bytes.iter().enumerate() {
        let value = if (CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4).contains(&index) {
            0
        } else {
            *byte
        };
        crc ^= u32::from(value);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
