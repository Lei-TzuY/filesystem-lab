use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::Superblock;
use crate::journal::{JournalEntry, TransactionId};
use crate::journal_region::load_journal_image;

/// Summary of one recovery pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RecoveryReport {
    pub committed_transactions: usize,
    pub home_writes: usize,
}

/// Replays committed durable journal transactions to their home blocks.
///
/// Recovery first loads and validates the entire persistent journal image. It then applies writes
/// only when their transaction's commit record is present. Home writes are issued in log order and
/// one device `flush` is performed after all replayed writes, establishing the home-location
/// durability boundary. An incomplete trailing transaction is ignored.
///
/// The operation is intentionally idempotent: if a crash or I/O failure occurs after only a prefix
/// of home writes, rerunning recovery from the still-durable journal simply overwrites those blocks
/// with the same contents and completes the remaining writes.
///
/// # Errors
///
/// Returns an error if the journal image is corrupt or malformed, if transaction ordering is
/// invalid, if a counter overflows, or if a home write or final durability flush fails.
pub fn recover_journal(
    device: &mut impl BlockDevice,
    superblock: Superblock,
) -> io::Result<RecoveryReport> {
    let entries = load_journal_image(device, superblock)?;
    let mut active: Option<TransactionId> = None;
    let mut pending = Vec::<(u64, [u8; BLOCK_SIZE])>::new();
    let mut report = RecoveryReport::default();

    for entry in entries {
        match entry {
            JournalEntry::Begin { txid } => {
                if active.is_some() {
                    return Err(invalid_data("nested journal transaction during recovery"));
                }
                active = Some(txid);
                pending.clear();
            }
            JournalEntry::Write { txid, block, data } => {
                if active != Some(txid) {
                    return Err(invalid_data(
                        "journal write does not match active recovery transaction",
                    ));
                }
                pending.push((block, *data));
            }
            JournalEntry::Commit { txid } => {
                if active != Some(txid) {
                    return Err(invalid_data(
                        "journal commit does not match active recovery transaction",
                    ));
                }
                for (block, data) in pending.drain(..) {
                    device.write_block(block, &data)?;
                    report.home_writes = report
                        .home_writes
                        .checked_add(1)
                        .ok_or_else(|| invalid_data("home write count overflowed usize"))?;
                }
                report.committed_transactions = report
                    .committed_transactions
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("transaction count overflowed usize"))?;
                active = None;
            }
        }
    }

    if report.home_writes != 0 {
        device.flush()?;
    }
    Ok(report)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::JournalLog;
    use crate::journal_region::store_journal_image;

    #[derive(Debug)]
    struct MemoryDevice {
        blocks: Vec<[u8; BLOCK_SIZE]>,
        writes: Vec<u64>,
        flushes: usize,
        fail_home_write_after: Option<usize>,
        home_writes_seen: usize,
        reserved_blocks: u64,
    }

    impl MemoryDevice {
        fn new(blocks: usize, reserved_blocks: u64) -> Self {
            Self {
                blocks: vec![[0_u8; BLOCK_SIZE]; blocks],
                writes: Vec::new(),
                flushes: 0,
                fail_home_write_after: None,
                home_writes_seen: 0,
                reserved_blocks,
            }
        }
    }

    impl BlockDevice for MemoryDevice {
        fn block_count(&self) -> u64 {
            u64::try_from(self.blocks.len()).expect("test device length fits u64")
        }

        fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
            let index = usize::try_from(block)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
            *buf = *self
                .blocks
                .get(index)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
            Ok(())
        }

        fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
            if block >= self.reserved_blocks {
                if self.fail_home_write_after == Some(self.home_writes_seen) {
                    return Err(io::Error::other("injected home write failure"));
                }
                self.home_writes_seen += 1;
            }
            let index = usize::try_from(block)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
            *self
                .blocks
                .get_mut(index)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))? =
                *buf;
            self.writes.push(block);
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn committed_transaction_replays_and_flushes_home_blocks() {
        let superblock = Superblock::with_journal_blocks(16, 3).unwrap();
        let mut device = MemoryDevice::new(16, superblock.reserved_blocks());
        let first = superblock.reserved_blocks();
        let second = first + 1;
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, first, [0x11; BLOCK_SIZE]).unwrap();
        log.write(txid, second, [0x22; BLOCK_SIZE]).unwrap();
        log.commit(txid).unwrap();
        store_journal_image(&mut device, superblock, log.entries()).unwrap();
        let flushes_before = device.flushes;

        let report = recover_journal(&mut device, superblock).unwrap();

        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 2);
        assert_eq!(device.blocks[usize::try_from(first).unwrap()], [0x11; BLOCK_SIZE]);
        assert_eq!(device.blocks[usize::try_from(second).unwrap()], [0x22; BLOCK_SIZE]);
        assert_eq!(device.flushes, flushes_before + 1);
    }

    #[test]
    fn uncommitted_tail_never_reaches_home_location() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let mut device = MemoryDevice::new(16, superblock.reserved_blocks());
        let home = superblock.reserved_blocks();
        let original = [0x33; BLOCK_SIZE];
        device.blocks[usize::try_from(home).unwrap()] = original;
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, home, [0x44; BLOCK_SIZE]).unwrap();
        store_journal_image(&mut device, superblock, log.entries()).unwrap();
        let flushes_before = device.flushes;

        let report = recover_journal(&mut device, superblock).unwrap();

        assert_eq!(report, RecoveryReport::default());
        assert_eq!(device.blocks[usize::try_from(home).unwrap()], original);
        assert_eq!(device.flushes, flushes_before);
    }

    #[test]
    fn replay_is_idempotent_after_partial_home_write_failure() {
        let superblock = Superblock::with_journal_blocks(16, 3).unwrap();
        let mut device = MemoryDevice::new(16, superblock.reserved_blocks());
        let first = superblock.reserved_blocks();
        let second = first + 1;
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, first, [0xaa; BLOCK_SIZE]).unwrap();
        log.write(txid, second, [0xbb; BLOCK_SIZE]).unwrap();
        log.commit(txid).unwrap();
        store_journal_image(&mut device, superblock, log.entries()).unwrap();

        device.fail_home_write_after = Some(1);
        assert_eq!(
            recover_journal(&mut device, superblock).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        assert_eq!(device.blocks[usize::try_from(first).unwrap()], [0xaa; BLOCK_SIZE]);
        assert_ne!(device.blocks[usize::try_from(second).unwrap()], [0xbb; BLOCK_SIZE]);

        device.fail_home_write_after = None;
        device.home_writes_seen = 0;
        let report = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(report.home_writes, 2);
        assert_eq!(device.blocks[usize::try_from(first).unwrap()], [0xaa; BLOCK_SIZE]);
        assert_eq!(device.blocks[usize::try_from(second).unwrap()], [0xbb; BLOCK_SIZE]);
    }

    #[test]
    fn empty_journal_is_a_noop_without_flush() {
        let superblock = Superblock::with_journal_blocks(8, 2).unwrap();
        let mut device = MemoryDevice::new(8, superblock.reserved_blocks());
        assert_eq!(
            recover_journal(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
        assert_eq!(device.flushes, 0);
    }
}
