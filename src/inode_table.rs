use std::collections::BTreeSet;
use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::Superblock;
use crate::inode_codec::{decode_inode, encode_inode, PersistedInode, INODE_RECORD_HEADER_LEN};

pub const INODE_TABLE_MAGIC: [u8; 8] = *b"FSLINOD\0";
pub const INODE_TABLE_VERSION: u32 = 1;
pub const INODE_TABLE_HEADER_LEN: usize = 32;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 8;
const FLAGS_OFFSET: usize = 12;
const PAYLOAD_BYTES_OFFSET: usize = 16;
const RECORD_COUNT_OFFSET: usize = 24;
const CHECKSUM_OFFSET: usize = 28;

/// Loads and validates the persistent inode table.
///
/// The region contains a checksummed concatenation of self-delimiting `INO1` records. Record IDs
/// must be unique. Zero padding after the encoded payload is mandatory so stale records cannot be
/// silently resurrected after a shorter rewrite.
///
/// # Errors
///
/// Returns `InvalidData` for malformed headers, checksum failure, non-zero padding, duplicate inode
/// IDs, torn/corrupt inode records, or inconsistent record counts. Device read failures propagate.
pub fn load_inode_table(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
) -> io::Result<Vec<PersistedInode>> {
    validate_geometry(device, superblock)?;
    let image = read_region(device, superblock)?;
    decode_image(&image)
}

/// Stores a complete inode-table snapshot and crosses the device flush durability boundary.
///
/// Tail blocks are issued before the header-bearing first block. The checksum plus mandatory zero
/// padding therefore turns a mixed old/new multi-block image into detectable corruption rather than
/// a silently accepted inode set.
///
/// # Errors
///
/// Returns `InvalidInput` for duplicate inode IDs, invalid inode records, or an image that exceeds
/// the reserved inode region. Device write/flush failures propagate.
pub fn store_inode_table(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inodes: &[PersistedInode],
) -> io::Result<()> {
    validate_geometry(device, superblock)?;
    let image = encode_image(superblock, inodes)?;
    write_region(device, superblock, &image)?;
    device.flush()
}

pub(crate) fn initialize_inode_table_region(
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
            "inode table device geometry does not match superblock",
        ));
    }
    if superblock.inode_blocks == 0 || superblock.inode_range().end > superblock.total_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid inode table reservation",
        ));
    }
    Ok(())
}

fn region_len(superblock: &Superblock) -> io::Result<usize> {
    let blocks = usize::try_from(superblock.inode_blocks).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "inode table block count is too large",
        )
    })?;
    blocks.checked_mul(BLOCK_SIZE).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "inode table byte size overflow",
        )
    })
}

fn encode_image(superblock: &Superblock, inodes: &[PersistedInode]) -> io::Result<Vec<u8>> {
    let mut seen = BTreeSet::new();
    let mut payload = Vec::new();
    for inode in inodes {
        if !seen.insert(inode.id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "inode table contains duplicate inode identifiers",
            ));
        }
        let record = encode_inode(inode)?;
        payload.extend_from_slice(&record);
    }

    let capacity = region_len(superblock)?;
    let used = INODE_TABLE_HEADER_LEN
        .checked_add(payload.len())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "inode image size overflow"))?;
    if used > capacity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inode table image exceeds reserved region",
        ));
    }

    let mut image = vec![0_u8; capacity];
    image[MAGIC_OFFSET..MAGIC_OFFSET + INODE_TABLE_MAGIC.len()].copy_from_slice(&INODE_TABLE_MAGIC);
    image[VERSION_OFFSET..VERSION_OFFSET + 4].copy_from_slice(&INODE_TABLE_VERSION.to_le_bytes());
    image[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&0_u32.to_le_bytes());
    image[PAYLOAD_BYTES_OFFSET..PAYLOAD_BYTES_OFFSET + 8].copy_from_slice(
        &u64::try_from(payload.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "inode payload too large"))?
            .to_le_bytes(),
    );
    image[RECORD_COUNT_OFFSET..RECORD_COUNT_OFFSET + 4].copy_from_slice(
        &u32::try_from(inodes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too many inode records"))?
            .to_le_bytes(),
    );
    image[INODE_TABLE_HEADER_LEN..used].copy_from_slice(&payload);
    let checksum = crc32(&image[..used]);
    image[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
    Ok(image)
}

fn decode_image(image: &[u8]) -> io::Result<Vec<PersistedInode>> {
    if image.len() < INODE_TABLE_HEADER_LEN {
        return Err(invalid_data("inode table region is shorter than header"));
    }
    if image[MAGIC_OFFSET..MAGIC_OFFSET + INODE_TABLE_MAGIC.len()] != INODE_TABLE_MAGIC {
        return Err(invalid_data("invalid inode table magic"));
    }
    if read_u32(image, VERSION_OFFSET) != INODE_TABLE_VERSION {
        return Err(invalid_data("unsupported inode table version"));
    }
    if read_u32(image, FLAGS_OFFSET) != 0 {
        return Err(invalid_data("inode table flags are non-zero"));
    }

    let payload_len = usize::try_from(read_u64(image, PAYLOAD_BYTES_OFFSET))
        .map_err(|_| invalid_data("inode table payload exceeds usize"))?;
    let record_count = usize::try_from(read_u32(image, RECORD_COUNT_OFFSET))
        .map_err(|_| invalid_data("inode table record count exceeds usize"))?;
    let used = INODE_TABLE_HEADER_LEN
        .checked_add(payload_len)
        .ok_or_else(|| invalid_data("inode table used length overflow"))?;
    if used > image.len() {
        return Err(invalid_data("inode table payload exceeds reserved region"));
    }
    if image[used..].iter().any(|byte| *byte != 0) {
        return Err(invalid_data("inode table trailing padding is non-zero"));
    }

    let stored_checksum = read_u32(image, CHECKSUM_OFFSET);
    let mut checksummed = image[..used].to_vec();
    checksummed[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].fill(0);
    if crc32(&checksummed) != stored_checksum {
        return Err(invalid_data("inode table checksum mismatch"));
    }

    let payload = &image[INODE_TABLE_HEADER_LEN..used];
    let mut offset = 0_usize;
    let mut inodes = Vec::with_capacity(record_count);
    let mut seen = BTreeSet::new();
    while offset < payload.len() {
        let remaining = &payload[offset..];
        if remaining.len() < INODE_RECORD_HEADER_LEN {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "torn inode record in table",
            ));
        }
        let total_len = usize::try_from(read_u32(remaining, 8))
            .map_err(|_| invalid_data("inode record length exceeds usize"))?;
        if total_len < INODE_RECORD_HEADER_LEN {
            return Err(invalid_data("inode record length is smaller than header"));
        }
        let end = offset
            .checked_add(total_len)
            .ok_or_else(|| invalid_data("inode record offset overflow"))?;
        if end > payload.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "torn inode record in table",
            ));
        }
        let inode = decode_inode(&payload[offset..end])?;
        if !seen.insert(inode.id) {
            return Err(invalid_data(
                "inode table contains duplicate inode identifiers",
            ));
        }
        inodes.push(inode);
        offset = end;
    }

    if inodes.len() != record_count {
        return Err(invalid_data("inode table record count mismatch"));
    }
    Ok(inodes)
}

fn read_region(device: &mut impl BlockDevice, superblock: &Superblock) -> io::Result<Vec<u8>> {
    let mut image = vec![0_u8; region_len(superblock)?];
    for (index, block) in superblock.inode_range().enumerate() {
        let start = index
            .checked_mul(BLOCK_SIZE)
            .ok_or_else(|| invalid_data("inode table offset overflow"))?;
        let end = start + BLOCK_SIZE;
        let target: &mut [u8; BLOCK_SIZE] = (&mut image[start..end])
            .try_into()
            .map_err(|_| invalid_data("inode table block slice mismatch"))?;
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
            "inode table image has invalid region length",
        ));
    }
    let blocks: Vec<u64> = superblock.inode_range().collect();
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
        io::Error::new(io::ErrorKind::InvalidInput, "inode table offset overflow")
    })?;
    let end = start + BLOCK_SIZE;
    let source: &[u8; BLOCK_SIZE] = image[start..end].try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "inode table block slice mismatch",
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
