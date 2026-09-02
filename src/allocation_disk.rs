use std::io;

use crate::allocation::BlockAllocator;
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::{allocation_bitmap_bytes, Superblock, ALLOCATION_IMAGE_HEADER_LEN};

pub const ALLOCATION_IMAGE_MAGIC: [u8; 8] = *b"FSLALOC\0";
pub const ALLOCATION_IMAGE_VERSION: u32 = 1;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 8;
const FLAGS_OFFSET: usize = 12;
const BITMAP_BYTES_OFFSET: usize = 16;
const CHECKSUM_OFFSET: usize = 24;
const RESERVED_OFFSET: usize = 28;
const HEADER_LEN: usize = 32;

/// Loads and validates the durable allocation bitmap into an in-memory allocator.
///
/// Reserved metadata is implicit in the superblock geometry and must remain clear in the bitmap.
/// Only data-block ownership is encoded persistently.
///
/// # Errors
///
/// Returns `InvalidData` for malformed image metadata, checksum mismatch, non-zero padding,
/// reserved/trailing bits, or allocator reconstruction failure. Device read failures are propagated.
pub fn load_allocator(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
) -> io::Result<BlockAllocator> {
    validate_device_geometry(device, superblock)?;
    let image = read_region(device, superblock)?;
    let bitmap = decode_image(&image, superblock)?;
    let mut allocator = BlockAllocator::new(superblock.total_blocks, superblock.reserved_blocks())
        .map_err(invalid_data)?;

    let data_blocks = superblock.total_blocks - superblock.reserved_blocks();
    for _ in 0..data_blocks {
        allocator.allocate().map_err(invalid_data)?;
    }

    for block in superblock.reserved_blocks()..superblock.total_blocks {
        if !bit_is_set(bitmap, block)? {
            allocator.free(block).map_err(invalid_data)?;
        }
    }
    allocator.validate().map_err(invalid_data)?;
    Ok(allocator)
}

/// Persists the allocator as a checksummed allocation image and crosses the device flush boundary.
///
/// Tail blocks are issued before the header-bearing first block. A failed write or flush therefore
/// never reports success, and a mixed old/new multi-block image is detected by the checksum on the
/// next load.
///
/// # Errors
///
/// Returns `InvalidInput` when allocator geometry disagrees with the superblock, and propagates
/// device write/flush failures or image-size conversion errors.
pub fn store_allocator(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    allocator: &BlockAllocator,
) -> io::Result<()> {
    validate_device_geometry(device, superblock)?;
    if allocator.total_blocks() != superblock.total_blocks
        || allocator.reserved_blocks() != superblock.reserved_blocks()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocator geometry does not match superblock",
        ));
    }
    allocator
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;

    let mut bitmap = vec![0_u8; bitmap_len(superblock)?];
    for block in superblock.reserved_blocks()..superblock.total_blocks {
        if allocator
            .is_owned(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?
        {
            set_bit(&mut bitmap, block)?;
        }
    }

    let image = encode_image(superblock, &bitmap)?;
    write_region(device, superblock, &image)?;
    device.flush()
}

pub(crate) fn initialize_allocation_region(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
) -> io::Result<()> {
    validate_device_geometry(device, superblock)?;
    let bitmap = vec![0_u8; bitmap_len(superblock)?];
    let image = encode_image(superblock, &bitmap)?;
    write_region(device, superblock, &image)
}

fn validate_device_geometry(device: &impl BlockDevice, superblock: &Superblock) -> io::Result<()> {
    if device.block_count() != superblock.total_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocation metadata device geometry does not match superblock",
        ));
    }
    Ok(())
}

fn bitmap_len(superblock: &Superblock) -> io::Result<usize> {
    let bytes = allocation_bitmap_bytes(superblock.total_blocks)?;
    usize::try_from(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocation bitmap is too large",
        )
    })
}

fn region_len(superblock: &Superblock) -> io::Result<usize> {
    let blocks = usize::try_from(superblock.allocation_blocks).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocation region block count is too large",
        )
    })?;
    blocks.checked_mul(BLOCK_SIZE).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocation region byte size overflow",
        )
    })
}

fn encode_image(superblock: &Superblock, bitmap: &[u8]) -> io::Result<Vec<u8>> {
    if bitmap.len() != bitmap_len(superblock)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocation bitmap length does not match filesystem geometry",
        ));
    }
    validate_bitmap_bits(bitmap, superblock)?;

    let mut image = vec![0_u8; region_len(superblock)?];
    let payload_end = HEADER_LEN.checked_add(bitmap.len()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocation image length overflow",
        )
    })?;
    if payload_end > image.len()
        || u64::try_from(HEADER_LEN).ok() != Some(ALLOCATION_IMAGE_HEADER_LEN)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocation image does not fit reserved region",
        ));
    }

    image[MAGIC_OFFSET..MAGIC_OFFSET + ALLOCATION_IMAGE_MAGIC.len()]
        .copy_from_slice(&ALLOCATION_IMAGE_MAGIC);
    image[VERSION_OFFSET..VERSION_OFFSET + 4]
        .copy_from_slice(&ALLOCATION_IMAGE_VERSION.to_le_bytes());
    image[FLAGS_OFFSET..FLAGS_OFFSET + 4].copy_from_slice(&0_u32.to_le_bytes());
    image[BITMAP_BYTES_OFFSET..BITMAP_BYTES_OFFSET + 8].copy_from_slice(
        &u64::try_from(bitmap.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bitmap length overflow"))?
            .to_le_bytes(),
    );
    image[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&crc32(bitmap).to_le_bytes());
    image[RESERVED_OFFSET..RESERVED_OFFSET + 4].copy_from_slice(&0_u32.to_le_bytes());
    image[HEADER_LEN..payload_end].copy_from_slice(bitmap);
    Ok(image)
}

fn decode_image<'a>(image: &'a [u8], superblock: &Superblock) -> io::Result<&'a [u8]> {
    if image.len() != region_len(superblock)? || image.len() < HEADER_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "allocation image has invalid region length",
        ));
    }
    if image[MAGIC_OFFSET..MAGIC_OFFSET + ALLOCATION_IMAGE_MAGIC.len()] != ALLOCATION_IMAGE_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid allocation image magic",
        ));
    }
    if read_u32_le(image, VERSION_OFFSET) != ALLOCATION_IMAGE_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported allocation image version",
        ));
    }
    if read_u32_le(image, FLAGS_OFFSET) != 0 || read_u32_le(image, RESERVED_OFFSET) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "allocation image reserved fields are non-zero",
        ));
    }

    let encoded_bytes = read_u64_le(image, BITMAP_BYTES_OFFSET);
    let expected_bytes = allocation_bitmap_bytes(superblock.total_blocks)?;
    if encoded_bytes != expected_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "allocation bitmap length does not match filesystem geometry",
        ));
    }
    let bitmap_bytes = usize::try_from(encoded_bytes).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "allocation bitmap is too large")
    })?;
    let payload_end = HEADER_LEN.checked_add(bitmap_bytes).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "allocation image length overflow",
        )
    })?;
    if payload_end > image.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "allocation bitmap exceeds reserved region",
        ));
    }

    let bitmap = &image[HEADER_LEN..payload_end];
    if read_u32_le(image, CHECKSUM_OFFSET) != crc32(bitmap) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "allocation bitmap checksum mismatch",
        ));
    }
    if image[payload_end..].iter().any(|byte| *byte != 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "allocation image padding is non-zero",
        ));
    }
    validate_bitmap_bits(bitmap, superblock)?;
    Ok(bitmap)
}

fn validate_bitmap_bits(bitmap: &[u8], superblock: &Superblock) -> io::Result<()> {
    for block in 0..superblock.reserved_blocks() {
        if bit_is_set(bitmap, block)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "allocation bitmap marks reserved metadata as data-owned",
            ));
        }
    }

    let bit_capacity = u64::try_from(bitmap.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bitmap length overflow"))?
        .checked_mul(8)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bitmap bit count overflow"))?;
    for block in superblock.total_blocks..bit_capacity {
        if bit_is_set(bitmap, block)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "allocation bitmap has non-zero trailing bits",
            ));
        }
    }
    Ok(())
}

fn read_region(device: &mut impl BlockDevice, superblock: &Superblock) -> io::Result<Vec<u8>> {
    let mut image = vec![0_u8; region_len(superblock)?];
    for (index, block) in superblock.allocation_range().enumerate() {
        let start = index.checked_mul(BLOCK_SIZE).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "allocation region offset overflow",
            )
        })?;
        let end = start + BLOCK_SIZE;
        let target: &mut [u8; BLOCK_SIZE] = image[start..end].try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "allocation block slice mismatch",
            )
        })?;
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
            "allocation image has invalid region length",
        ));
    }

    let blocks: Vec<u64> = superblock.allocation_range().collect();
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
            "allocation region offset overflow",
        )
    })?;
    let end = start + BLOCK_SIZE;
    let source: &[u8; BLOCK_SIZE] = image[start..end].try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocation block slice mismatch",
        )
    })?;
    device.write_block(block, source)
}

fn bit_is_set(bitmap: &[u8], block: u64) -> io::Result<bool> {
    let byte_index = usize::try_from(block / 8)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bitmap index overflow"))?;
    let bit_index = u32::try_from(block % 8)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bitmap bit overflow"))?;
    let byte = bitmap
        .get(byte_index)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bitmap index out of range"))?;
    Ok(byte & (1_u8 << bit_index) != 0)
}

fn set_bit(bitmap: &mut [u8], block: u64) -> io::Result<()> {
    let byte_index = usize::try_from(block / 8)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bitmap index overflow"))?;
    let bit_index = u32::try_from(block % 8)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bitmap bit overflow"))?;
    let byte = bitmap
        .get_mut(byte_index)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bitmap index out of range"))?;
    *byte |= 1_u8 << bit_index;
    Ok(())
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64_le(bytes: &[u8], offset: usize) -> u64 {
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
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}
