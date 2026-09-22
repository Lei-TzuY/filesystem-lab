use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};
use crate::format::{Superblock, SUPERBLOCK_BLOCK};
use crate::journal::{JournalEntry, TransactionId};
use crate::journal_codec::{decode_entries, encode_entries};

const REGION_MAGIC_V1: [u8; 4] = *b"JRG1";
const REGION_MAGIC_V2: [u8; 4] = *b"JRG2";
const REGION_VERSION_V1: u16 = 1;
const REGION_VERSION_V2: u16 = 2;
const REGION_STATE_EMPTY: u16 = 0;
const REGION_STATE_ACTIVE: u16 = 1;
const HEADER_SIZE: usize = 32;
const STATE_OFFSET: usize = 6;
const CHECKSUM_OFFSET: usize = 16;
const RESERVED_OFFSET: usize = 20;

/// Stores one bounded journal image inside the superblock-reserved journal region.
///
/// Version 2 uses a checksummed first-block anchor with explicit empty/active state. Before a new
/// active image is published, a durable empty anchor is established. Tail blocks are then staged and
/// flushed before the active anchor is allowed to become durable. This ordering remains correct when
/// successful `write_block` calls reach stable storage before a later `flush`.
///
/// A new non-empty journal image may only be published when the current reservation is empty. This
/// prevents a later transaction from overwriting the only durable recovery source for an earlier
/// committed transaction whose home replay has not completed. Callers that encounter a non-empty
/// journal must recover and checkpoint it before retrying the mutation.
///
/// Journal writes may target data blocks or the allocation/inode/directory metadata home regions.
/// They may never target the superblock or journal reservation itself.
///
/// # Errors
///
/// Returns `WouldBlock` when a non-empty journal image already occupies the reservation, including
/// attempts to clear it through this publication API. Recovery/checkpoint owns active-log removal.
/// Returns an error if the superblock does not
/// describe this device, the existing or replacement journal image is corrupt, the reservation is
/// malformed or too large to address, an entry targets a forbidden/out-of-range block, transaction
/// ordering is malformed, the encoded stream does not fit, or an underlying read/write/flush fails.
pub fn store_journal_image(
    device: &mut impl BlockDevice,
    superblock: Superblock,
    entries: &[JournalEntry],
) -> io::Result<()> {
    validate_region(device, superblock)?;
    validate_entries(superblock, entries)?;

    if !load_journal_image(device, superblock)?.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "journal contains an older transaction; recover and checkpoint before replacement",
        ));
    }

    if entries.is_empty() {
        return store_empty_journal_anchor(device, superblock);
    }

    // Establish a durable empty anchor before any tail block is staged. The BlockDevice contract
    // permits an issued write to become durable before flush, so a crash during tail publication
    // must still decode as an empty journal rather than as an old header plus new tail bytes.
    store_empty_journal_anchor(device, superblock)?;

    let payload = encode_entries(entries)?;
    let capacity = region_capacity(superblock)?;
    let used = HEADER_SIZE
        .checked_add(payload.len())
        .ok_or_else(|| invalid_input("journal region image length overflow"))?;
    if used > capacity {
        return Err(invalid_input("journal image exceeds reserved region"));
    }

    let mut region = vec![0_u8; capacity];
    region[0..4].copy_from_slice(&REGION_MAGIC_V2);
    region[4..6].copy_from_slice(&REGION_VERSION_V2.to_le_bytes());
    region[STATE_OFFSET..STATE_OFFSET + 2].copy_from_slice(&REGION_STATE_ACTIVE.to_le_bytes());
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
    if block_count > 1 {
        // Once the active anchor is allowed to become durable, every referenced tail byte must
        // already be durable. This flush is therefore a publication barrier, not merely a final
        // completion flush.
        device.flush()?;
    }
    write_region_block(device, superblock, &region, 0)?;
    device.flush()
}

/// Publishes the v2 empty journal anchor after the caller has made any replayed home state durable.
///
/// Only the header-bearing first journal block is rewritten. Older tail bytes deliberately remain
/// untouched and are non-authoritative while the empty anchor is present. A successful block write
/// may become durable before the following flush; either the previous complete active anchor or the
/// new complete empty anchor is therefore recoverable under the repository's whole-block model.
pub(crate) fn store_empty_journal_anchor(
    device: &mut impl BlockDevice,
    superblock: Superblock,
) -> io::Result<()> {
    validate_region(device, superblock)?;

    let mut block = [0_u8; BLOCK_SIZE];
    block[0..4].copy_from_slice(&REGION_MAGIC_V2);
    block[4..6].copy_from_slice(&REGION_VERSION_V2.to_le_bytes());
    block[STATE_OFFSET..STATE_OFFSET + 2].copy_from_slice(&REGION_STATE_EMPTY.to_le_bytes());
    let checksum = crc32(&block[..HEADER_SIZE]);
    block[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());

    device.write_block(superblock.journal_start, &block)?;
    device.flush()
}

/// Loads and validates the bounded journal image from the reserved journal region.
///
/// Completely zeroed reservations remain accepted for freshly formatted filesystems. Version-1
/// images remain readable for compatibility. Version 2 adds an explicit empty anchor; when that
/// anchor is present, stale bytes in later journal blocks are intentionally ignored because they
/// are outside the authoritative log state.
///
/// # Errors
///
/// Returns an error if the superblock/device relation is invalid, region I/O fails, the persistent
/// header/version/state/reserved bytes are invalid, the payload length exceeds the reservation,
/// active-image trailing padding is non-zero, the checksum fails, a record is corrupt/torn,
/// transaction ordering is malformed, or a write entry targets a forbidden/out-of-range block.
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
        let mut block_data = [0_u8; BLOCK_SIZE];
        device.read_block(block, &mut block_data)?;
        region[start..end].copy_from_slice(&block_data);
    }

    if region.iter().all(|byte| *byte == 0) {
        return Ok(Vec::new());
    }

    let magic: [u8; 4] = region[0..4]
        .try_into()
        .map_err(|_| invalid_data("journal region magic is malformed"))?;
    let version = u16::from_le_bytes([region[4], region[5]]);
    let state = u16::from_le_bytes([region[STATE_OFFSET], region[STATE_OFFSET + 1]]);
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

    match (magic, version) {
        (REGION_MAGIC_V1, REGION_VERSION_V1) => {
            if state != 0 {
                return Err(invalid_data("unsupported journal region v1 flags"));
            }
        }
        (REGION_MAGIC_V2, REGION_VERSION_V2) => match state {
            REGION_STATE_EMPTY => {
                if payload_len != 0 {
                    return Err(invalid_data("empty journal anchor has a payload"));
                }
                if region[HEADER_SIZE..BLOCK_SIZE]
                    .iter()
                    .any(|byte| *byte != 0)
                {
                    return Err(invalid_data("empty journal anchor block padding is non-zero"));
                }
                let expected_checksum = u32::from_le_bytes(
                    region[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4]
                        .try_into()
                        .map_err(|_| invalid_data("journal region checksum field is malformed"))?,
                );
                let mut header = region[..HEADER_SIZE].to_vec();
                header[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].fill(0);
                if crc32(&header) != expected_checksum {
                    return Err(invalid_data("journal empty-anchor checksum mismatch"));
                }
                return Ok(Vec::new());
            }
            REGION_STATE_ACTIVE => {}
            _ => return Err(invalid_data("unsupported journal region v2 state")),
        },
        _ => return Err(invalid_data("unsupported journal region magic/version")),
    }

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
                let allocation_home = superblock.allocation_range().contains(block);
                let inode_home = superblock.inode_range().contains(block);
                let directory_home = superblock.directory_range().contains(block);
                let data_home =
                    *block >= superblock.reserved_blocks() && *block < superblock.total_blocks;
                if !allocation_home && !inode_home && !directory_home && !data_home {
                    return Err(invalid_data(
                        "journal write targets forbidden or invalid block",
                    ));
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
    let index_u64 = u64::try_from(index).map_err(|_| invalid_input("journal index exceeds u64"))?;
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

        assert_eq!(device.writes, vec![1, 2, 1]);
        assert_eq!(device.flushes, 3);
        assert_eq!(
            load_journal_image(&mut device, superblock).unwrap(),
            entries
        );
    }

    #[test]
    fn rejects_replacement_while_an_older_journal_image_is_present() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let first = sample_entries(superblock);
        let mut second_log = JournalLog::new();
        let txid = second_log.begin().unwrap();
        second_log
            .write(txid, superblock.reserved_blocks() + 1, [0xa5; BLOCK_SIZE])
            .unwrap();
        second_log.commit(txid).unwrap();
        let mut device = MemoryDevice::new(16);

        store_journal_image(&mut device, superblock, &first).unwrap();
        let writes_before = device.writes.clone();
        let flushes_before = device.flushes;

        assert_eq!(
            store_journal_image(&mut device, superblock, second_log.entries())
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(device.writes, writes_before);
        assert_eq!(device.flushes, flushes_before);
        assert_eq!(load_journal_image(&mut device, superblock).unwrap(), first);
    }

    #[test]
    fn empty_store_cannot_discard_an_active_journal() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let entries = sample_entries(superblock);
        let mut device = MemoryDevice::new(16);
        store_journal_image(&mut device, superblock, &entries).unwrap();
        let writes_before = device.writes.clone();
        let flushes_before = device.flushes;

        assert_eq!(
            store_journal_image(&mut device, superblock, &[])
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        assert_eq!(device.writes, writes_before);
        assert_eq!(device.flushes, flushes_before);
        assert_eq!(load_journal_image(&mut device, superblock).unwrap(), entries);
    }

    #[test]
    fn version_one_active_image_remains_readable() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let entries = sample_entries(superblock);
        let payload = encode_entries(&entries).unwrap();
        let capacity = region_capacity(superblock).unwrap();
        let used = HEADER_SIZE + payload.len();
        let mut region = vec![0_u8; capacity];
        region[0..4].copy_from_slice(&REGION_MAGIC_V1);
        region[4..6].copy_from_slice(&REGION_VERSION_V1.to_le_bytes());
        region[8..16].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        region[HEADER_SIZE..used].copy_from_slice(&payload);
        let checksum = crc32(&region[..used]);
        region[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());

        let mut device = MemoryDevice::new(16);
        for (index, block) in superblock.journal_range().enumerate() {
            let start = index * BLOCK_SIZE;
            let end = start + BLOCK_SIZE;
            device.blocks[usize::try_from(block).unwrap()]
                .copy_from_slice(&region[start..end]);
        }

        assert_eq!(load_journal_image(&mut device, superblock).unwrap(), entries);
    }

    #[test]
    fn zeroed_fresh_region_is_empty() {
        let superblock = Superblock::with_journal_blocks(8, 2).unwrap();
        let mut device = MemoryDevice::new(8);
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn region_checksum_detects_cross_block_corruption() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let entries = sample_entries(superblock);
        let mut device = MemoryDevice::new(16);
        store_journal_image(&mut device, superblock, &entries).unwrap();
        device.blocks[2][100] ^= 0xff;

        assert_eq!(
            load_journal_image(&mut device, superblock)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn empty_anchor_ignores_stale_tail_blocks() {
        let superblock = Superblock::with_journal_blocks(8, 2).unwrap();
        let mut device = MemoryDevice::new(8);
        store_journal_image(&mut device, superblock, &[]).unwrap();
        device.blocks[2][BLOCK_SIZE - 1] = 1;

        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn active_image_still_rejects_non_zero_trailing_padding() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let entries = sample_entries(superblock);
        let mut device = MemoryDevice::new(16);
        store_journal_image(&mut device, superblock, &entries).unwrap();
        device.blocks[2][BLOCK_SIZE - 1] = 1;

        assert_eq!(
            load_journal_image(&mut device, superblock)
                .unwrap_err()
                .kind(),
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
    fn writes_cannot_target_superblock_or_journal_metadata() {
        let superblock = Superblock::with_journal_blocks(8, 2).unwrap();
        for forbidden in [SUPERBLOCK_BLOCK, superblock.journal_start] {
            let mut log = JournalLog::new();
            let txid = log.begin().unwrap();
            log.write(txid, forbidden, [1; BLOCK_SIZE]).unwrap();
            log.commit(txid).unwrap();
            let mut device = MemoryDevice::new(8);

            assert_eq!(
                store_journal_image(&mut device, superblock, log.entries())
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn writes_may_target_allocation_metadata_home_blocks() {
        let superblock = Superblock::with_journal_blocks(8, 2).unwrap();
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, superblock.allocation_start, [0x7c; BLOCK_SIZE])
            .unwrap();
        log.commit(txid).unwrap();
        let mut device = MemoryDevice::new(8);

        store_journal_image(&mut device, superblock, log.entries()).unwrap();
        assert_eq!(
            load_journal_image(&mut device, superblock).unwrap(),
            log.entries()
        );
    }

    #[test]
    fn writes_may_target_inode_metadata_home_blocks() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, superblock.inode_start, [0x6d; BLOCK_SIZE])
            .unwrap();
        log.commit(txid).unwrap();
        let mut device = MemoryDevice::new(16);

        store_journal_image(&mut device, superblock, log.entries()).unwrap();
        assert_eq!(
            load_journal_image(&mut device, superblock).unwrap(),
            log.entries()
        );
    }

    #[test]
    fn writes_may_target_directory_metadata_home_blocks() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, superblock.directory_start, [0x4f; BLOCK_SIZE])
            .unwrap();
        log.commit(txid).unwrap();
        let mut device = MemoryDevice::new(16);

        store_journal_image(&mut device, superblock, log.entries()).unwrap();
        assert_eq!(
            load_journal_image(&mut device, superblock).unwrap(),
            log.entries()
        );
    }

    #[test]
    fn device_size_must_match_superblock() {
        let superblock = Superblock::with_journal_blocks(8, 2).unwrap();
        let mut device = MemoryDevice::new(9);
        assert_eq!(
            load_journal_image(&mut device, superblock)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
