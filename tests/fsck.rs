use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::format_device;
use filesystem_lab::fsck::check_device;
use filesystem_lab::journal::JournalLog;
use filesystem_lab::journal_region::store_journal_image;

#[derive(Debug)]
struct MemoryDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
    writes: usize,
    flushes: usize,
}

impl MemoryDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0_u8; BLOCK_SIZE]; blocks],
            writes: 0,
            flushes: 0,
        }
    }
}

impl BlockDevice for MemoryDevice {
    fn block_count(&self) -> u64 {
        u64::try_from(self.blocks.len()).expect("test device size fits u64")
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
        let source = self
            .blocks
            .get(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
        *buf = *source;
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
        let destination = self
            .blocks
            .get_mut(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
        *destination = *buf;
        self.writes += 1;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn fresh_device_passes_without_mutation() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let writes_before = device.writes;
    let flushes_before = device.flushes;

    let report = check_device(&mut device).unwrap();

    assert_eq!(report.total_blocks, 16);
    assert_eq!(report.reserved_blocks, superblock.reserved_blocks());
    assert_eq!(report.data_blocks, 12);
    assert_eq!(report.allocated_blocks, 0);
    assert_eq!(report.free_blocks, 12);
    assert_eq!(report.journal_entries, 0);
    assert_eq!(report.journal_writes, 0);
    assert_eq!(report.committed_transactions, 0);
    assert_eq!(report.pending_transaction, None);
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
}

#[test]
fn reports_committed_and_crash_incomplete_transactions() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let mut log = JournalLog::new();

    let committed = log.begin().unwrap();
    log.write(committed, superblock.reserved_blocks(), [0x11; BLOCK_SIZE])
        .unwrap();
    log.commit(committed).unwrap();

    let pending = log.begin().unwrap();

    store_journal_image(&mut device, superblock, log.entries()).unwrap();
    let writes_before = device.writes;
    let flushes_before = device.flushes;

    let report = check_device(&mut device).unwrap();

    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.pending_transaction, Some(pending));
    assert_eq!(report.journal_writes, 1);
    assert_eq!(device.writes, writes_before);
    assert_eq!(device.flushes, flushes_before);
}

#[test]
fn detects_superblock_corruption_before_journal_scan() {
    let mut device = MemoryDevice::new(16);
    format_device(&mut device).unwrap();
    device.blocks[0][BLOCK_SIZE - 1] = 1;

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("fsck superblock"));
}

#[test]
fn detects_allocation_corruption_before_journal_scan() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let allocation_block = usize::try_from(superblock.allocation_start).unwrap();
    device.blocks[allocation_block][32] ^= 0x80;

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("fsck allocation"));
}

#[test]
fn detects_cross_block_journal_corruption() {
    let mut device = MemoryDevice::new(16);
    let superblock = format_device(&mut device).unwrap();
    let mut log = JournalLog::new();
    let txid = log.begin().unwrap();
    log.write(txid, superblock.reserved_blocks(), [0x5a; BLOCK_SIZE])
        .unwrap();
    log.commit(txid).unwrap();
    store_journal_image(&mut device, superblock, log.entries()).unwrap();

    let second_journal_block = usize::try_from(superblock.journal_start + 1).unwrap();
    device.blocks[second_journal_block][128] ^= 0xff;

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("fsck journal"));
}
