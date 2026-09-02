use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};
use crate::format::{Superblock, SUPERBLOCK_BLOCK};
use crate::journal::{JournalEntry, TransactionId};
use crate::journal_codec::{decode_entries, encode_entries};

const REGION_MAGIC: [u8; 4] = *b"JRG1";
const REGION_VERSION: u16 = 1;
const HEADER_SIZE: usize = 32;
const CHECKSUM_OFFSET: usize = 16;
const RESERVED_OFFSET: usize = 20;

/// Stores one bounded journal image inside the superblock-reserved journal region.
///
/// The image is deterministic and self-delimiting: a 32-byte region header records the encoded
/// journal-stream length and a CRC-32 covering the header plus payload. Unused bytes in the
/// reservation are zeroed. Tail blocks are written before the first journal block, so the block
/// containing the region header is the final on-device anchor before `flush` establishes the
/// durability boundary.
///
/// # Errors
///
/// Returns an error if the superblock does not describe this device, the journal reservation is
/// malformed or too large to address, an entry targets reserved/out-of-range metadata, transaction
/// ordering is malformed, the encoded stream does not fit, or an underlying write/flush fails.
pub fn store_journal_image(
    device: &mut impl BlockDevice,
    superblock: Superblock,
    entries: &[JournalEntry],
) -> io::Result<()> {
    validate_region(device, superblock)?;
    validate_entries(superblock, entries)?;

    let payload = encode_entries(entries)?;
    let capacity = region_capacity(superblock)?;
    let used = HEADER_SIZE
        .checked_add(payload.len())
        .ok_or_else(|| invalid_input("journal region image length overflow"))?;
    if used > capacity {
        return Err(invalid_input("journal image exceeds reserved region"));
    }

    let mut region = vec![0_u8; capacity];
    region[0..4].copy_from_slice(&REGION_MAGIC);
    region[4..6].copy_from_slice(&REGION_VERSION.to_le_bytes());
    region[6..8].copy_from_slice(&0_u16.to_le_bytes());
    let payload_len = u64::try_from(payload.len())
        .map_err(|_| invalid_input("journal payload length exceeds u64"))?;
    region[8..16].copy_from_slice(&payload_len.to_le_bytes());
    region[HEADER_SIZE..used].copy_from_slice(&payload);

    let checksum = crc32(&region[..used]);
    region[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());

    let block_count = usize::try_from(superblock.journal_blocks)
        .map_err(|_| invalid_input("journal block count exceeds usize"))?;
    for index in (1..block_count).rev() {
        write_region_block(device, superblock, &region, index)?;
    }
    write_region_block(device, superblock, &region, 0)?;
    device.flush()
}

/// Loads and validates the bounded journal image from the reserved journal region.
///
/// A completely zeroed reservation is treated as an empty journal, which is the state of a newly
/// formatted filesystem before the first journal image is stored. Any non-zero malformed image is
/// rejected rather than guessed or truncated.
///
/// # Errors
///
/// Returns an error if the superblock/device relation is invalid, region I/O fails, the persistent
/// header/version/flags/reserved bytes are invalid, the payload length exceeds the reservation,
/// trailing padding is non-zero, the checksum fails, a record is corrupt/torn, transaction ordering
/// is malformed, or a write entry targets reserved/out-of-range metadata.
pub fn load_journal_image(
    device: &mut impl BlockDevice,
    superblock: Superblock,
) -> io::Result<Vec<JournalEntry>> {
    validate_region(device, superblock)?;
    let capacity = region_capacity(superblock)?;
    let mut region = vec![0_u8; capacity];

    let block_count = usize::try_from(superblock.journal_blocks)
        .map_err(|_| invalid_data("journal block count exceeds usize"))?;
    for index in 0..block_count {
        let index_u64 =
            u64::try_from(index).map_err(|_| invalid_data("journal index exceeds u64"))?;
        let block = superblock
            .journal_start
            .checked_add(index_u64)
            .ok_or_else(|| invalid_data("journal block index overflow"))?;
        let start = index
            .checked_mul(BLOCK_SIZE)
            .ok_or_else(|| invalid_data("journal byte offset overflow"))?;
        let end = start
            .checked_add(BLOCK_SIZE)
            .ok_or_else(|| invalid_data("journal byte range overflow"))?;
        let chunk: &mut [u8; BLOCK_SIZE] = region[start..end]
            .try_into()
            .map_err(|_| invalid_data("journal block slice has invalid size"))?;
        device.read_block(block, chunk)?;
    }

    if region.iter().all(|byte| *byte == 0) {
        return Ok(Vec::new());
    }
    if region[0..4] != REGION_MAGIC {
        return Err(invalid_data("invalid journal region magic"));
    }
    let version = u16::from_le_bytes([region[4], region[5]]);
    if version != REGION_VERSION {
        return Err(invalid_data("unsupported journal region version"));
    }
    if region[6] != 0 || region[7] != 0 {
        return Err(invalid_data("unsupported journal region flags"));
    }
    if region[RESERVED_OFFSET..HEADER_SIZE]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(invalid_data("journal region reserved bytes are non-zero"));
    }

    let payload_len_u64 = u64::from_le_bytes(
        region[8..16]
            .try_into()
            .map_err(|_| invalid_data("journal payload length field is malformed"))?,
    );
    let payload_len = usize::try_from(payload_len_u64)
        .map_err(|_| invalid_data("journal payload length exceeds usize"))?;
    let used = HEADER_SIZE
        .checked_add(payload_len)
        .ok_or_else(|| invalid_data("journal region used length overflow"))?;
    if used > capacity {
        return Err(invalid_data("journal payload exceeds reserved region"));
    }
    if region[used..].iter().any(|byte| *byte != 0) {
        return Err(invalid_data("journal region trailing padding is non-zero"));
    }

    let expected_checksum = u32::from_le_bytes(
        region[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4]
            .try_into()
            .map_err(|_| invalid_data("journal region checksum field is malformed"))?,
    );
    let mut checksummed = region[..used].to_vec();
    checksummed[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].fill(0);
    if crc32(&checksummed) != expected_checksum {
        return Err(invalid_data("journal region checksum mismatch"));
    }

    let entries = decode_entries(&region[HEADER_SIZE..used])?;
    validate_entries(superblock, &entries)?;
    Ok(entries)
}

fn validate_region(device: &impl BlockDevice, superblock: Superblock) -> io::Result<()> {
    if superblock.total_blocks != device.block_count() {
        return Err(invalid_input(
            "superblock block count does not match journal device",
        ));
    }
    if superblock.journal_start != SUPERBLOCK_BLOCK + 1 || superblock.journal_blocks == 0 {
        return Err(invalid_input("invalid journal reservation"));
    }
    let journal_end = superblock
        .journal_start
        .checked_add(superblock.journal_blocks)
        .ok_or_else(|| invalid_input("journal block range overflow"))?;
    if journal_end > superblock.total_blocks {
        return Err(invalid_input("journal reservation exceeds filesystem size"));
    }
    Ok(())
}

fn validate_entries(superblock: Superblock, entries: &[JournalEntry]) -> io::Result<()> {
    let mut active: Option<TransactionId> = None;
    for entry in entries {
        match entry {
            JournalEntry::Begin { txid } => {
                if active.is_some() {
                    return Err(invalid_data("nested journal transaction"));
                }
                active = Some(*txid);
            }
            JournalEntry::Write { txid, block, .. } => {
                if active != Some(*txid) {
                    return Err(invalid_data(
                        "journal write does not match active transaction",
                    ));
                }
                if *block < superblock.reserved_blocks() || *block >= superblock.total_blocks {
                    return Err(invalid_data("journal write targets reserved or invalid block"));
                }
            }
            JournalEntry::Commit { txid } => {
                if active != Some(*txid) {
                    return Err(invalid_data(
                        "journal commit does not match active transaction",
                    ));
                }
                active = None;
            }
        }
    }
    Ok(())
}

fn region_capacity(superblock: Superblock) -> io::Result<usize> {
    let bytes = superblock
        .journal_blocks
        .checked_mul(BLOCK_SIZE_U64)
        .ok_or_else(|| invalid_input("journal region byte size overflow"))?;
    usize::try_from(bytes).map_err(|_| invalid_input("journal region byte size exceeds usize"))
}

fn write_region_block(
    device: &mut impl BlockDevice,
    superblock: Superblock,
    region: &[u8],
    index: usize,
) -> io::Result<()> {
    let index_u64 =
        u64::try_from(index).map_err(|_| invalid_input("journal index exceeds u64"))?;
    let block = superblock
        .journal_start
        .checked_add(index_u64)
        .ok_or_else(|| invalid_input("journal block index overflow"))?;
    let start = index
        .checked_mul(BLOCK_SIZE)
        .ok_or_else(|| invalid_input("journal byte offset overflow"))?;
    let end = start
        .checked_add(BLOCK_SIZE)
        .ok_or_else(|| invalid_input("journal byte range overflow"))?;
    let chunk: &[u8; BLOCK_SIZE] = region[start..end]
        .try_into()
        .map_err(|_| invalid_input("journal block slice has invalid size"))?;
    device.write_block(block, chunk)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
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

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::JournalLog;

    #[derive(Debug)]
    struct MemoryDevice {
        blocks: Vec<[u8; BLOCK_SIZE]>,
        writes: Vec<u64>,
        flushes: usize,
    }

    impl MemoryDevice {
        fn new(blocks: usize) -> Self {
            Self {
                blocks: vec![[0_u8; BLOCK_SIZE]; blocks],
                writes: Vec::new(),
                flushes: 0,
            }
        }
    }

    impl BlockDevice for MemoryDevice {
        fn block_count(&self) -> u64 {
            u64::try_from(self.blocks.len()).unwrap()
        }

        fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
            let index = usize::try_from(block).map_err(|_| invalid_input("block exceeds usize"))?;
            let source = self
                .blocks
                .get(index)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
            *buf = *source;
            Ok(())
        }

        fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
            let index = usize::try_from(block).map_err(|_| invalid_input("block exceeds usize"))?;
            let destination = self
                .blocks
                .get_mut(index)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
            *destination = *buf;
            self.writes.push(block);
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    fn sample_entries(superblock: Superblock) -> Vec<JournalEntry> {
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, superblock.reserved_blocks(), [0x5a; BLOCK_SIZE])
            .unwrap();
        log.commit(txid).unwrap();
        log.entries().to_vec()
    }

    #[test]
    fn round_trip_spans_blocks_and_flushes_with_header_block_last() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let entries = sample_entries(superblock);
        let mut device = MemoryDevice::new(16);

        store_journal_image(&mut device, superblock, &entries).unwrap();

        assert_eq!(device.writes, vec![2, 1]);
        assert_eq!(device.flushes, 1);
        assert_eq!(load_journal_image(&mut device, superblock).unwrap(), entries);
    }

    #[test]
    fn zeroed_fresh_region_is_empty() {
        let superblock = Superblock::with_journal_blocks(8, 2).unwrap();
        let mut device = MemoryDevice::new(8);
        assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
    }

    #[test]
    fn region_checksum_detects_cross_block_corruption() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let entries = sample_entries(superblock);
        let mut device = MemoryDevice::new(16);
        store_journal_image(&mut device, superblock, &entries).unwrap();
        device.blocks[2][100] ^= 0xff;

        assert_eq!(
            load_journal_image(&mut device, superblock).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn stale_non_zero_padding_is_rejected() {
        let superblock = Superblock::with_journal_blocks(8, 2).unwrap();
        let mut device = MemoryDevice::new(8);
        store_journal_image(&mut device, superblock, &[]).unwrap();
        device.blocks[2][BLOCK_SIZE - 1] = 1;

        assert_eq!(
            load_journal_image(&mut device, superblock).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn image_must_fit_reserved_region() {
        let superblock = Superblock::with_journal_blocks(8, 1).unwrap();
        let entries = sample_entries(superblock);
        let mut device = MemoryDevice::new(8);
        assert_eq!(
            store_journal_image(&mut device, superblock, &entries)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn writes_cannot_target_reserved_metadata() {
        let superblock = Superblock::with_journal_blocks(8, 2).unwrap();
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, superblock.journal_start, [1; BLOCK_SIZE])
            .unwrap();
        log.commit(txid).unwrap();
        let mut device = MemoryDevice::new(8);

        assert_eq!(
            store_journal_image(&mut device, superblock, log.entries())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn device_size_must_match_superblock() {
        let superblock = Superblock::with_journal_blocks(8, 2).unwrap();
        let mut device = MemoryDevice::new(9);
        assert_eq!(
            load_journal_image(&mut device, superblock).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
