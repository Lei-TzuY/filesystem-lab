use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::store_allocator;
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::Superblock;
use crate::journal::JournalLog;
use crate::journal_region::store_journal_image;
use crate::recovery::{recover_journal, RecoveryReport};
use crate::transaction_image::CaptureDevice;

/// Persists one complete allocator image through the existing write-ahead log and recovery path.
///
/// The desired allocator image is first rendered into an isolated capture device using the normal
/// allocation-image encoder. Those exact home blocks are then recorded as one journal transaction.
/// `store_journal_image` crosses the journal durability boundary before `recover_journal` writes the
/// committed image to the allocation home region and crosses the home-location durability boundary.
///
/// This is intentionally a bounded whole-image transaction. If the reserved journal cannot contain
/// all allocation-image blocks plus transaction framing, the operation fails rather than splitting
/// one logical allocator update across multiple commits.
///
/// # Errors
///
/// Returns `InvalidInput` when device/allocator geometry disagrees with the superblock or when the
/// bounded journal cannot hold the complete allocation image. Journal, home-write, and flush errors
/// are propagated. A returned error never claims that the requested allocator state is durable.
pub fn store_allocator_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    allocator: &BlockAllocator,
) -> io::Result<RecoveryReport> {
    if device.block_count() != superblock.total_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "journaled allocator device geometry does not match superblock",
        ));
    }

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, allocator)?;

    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for block in superblock.allocation_range() {
        let data = capture.take_rendered_block(
            block,
            "allocator image did not render every allocation metadata block",
        )?;
        log.write(txid, block, data)?;
    }
    capture.ensure_empty("allocator image rendered outside allocation metadata region")?;
    log.commit(txid)?;

    store_journal_image(device, *superblock, log.entries())?;
    let report = recover_journal(device, *superblock)?;
    let expected_writes = usize::try_from(superblock.allocation_blocks).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "allocation block count exceeds usize",
        )
    })?;
    if report.committed_transactions != 1 || report.home_writes != expected_writes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journaled allocator recovery report does not match one complete transaction",
        ));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocation_disk::load_allocator;
    use crate::format::format_device;
    use crate::journal::JournalLog;
    use crate::journal_region::store_journal_image;

    #[derive(Debug)]
    struct FaultDevice {
        blocks: Vec<[u8; BLOCK_SIZE]>,
        flushes: usize,
        fail_once_on: Option<u64>,
    }

    impl FaultDevice {
        fn new(blocks: usize) -> Self {
            Self {
                blocks: vec![[0_u8; BLOCK_SIZE]; blocks],
                flushes: 0,
                fail_once_on: None,
            }
        }
    }

    impl BlockDevice for FaultDevice {
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
            if self.fail_once_on == Some(block) {
                self.fail_once_on = None;
                return Err(io::Error::other("injected home allocation write failure"));
            }
            let index = usize::try_from(block)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
            *self
                .blocks
                .get_mut(index)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))? =
                *buf;
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    #[test]
    fn journaled_allocator_update_crosses_log_then_home_durability_boundaries() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let mut allocator = load_allocator(&mut device, &superblock).unwrap();
        let first = allocator.allocate().unwrap();
        let second = allocator.allocate().unwrap();
        allocator.free(first).unwrap();
        let flushes_before = device.flushes;

        let report = store_allocator_journaled(&mut device, &superblock, &allocator).unwrap();

        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 1);
        assert_eq!(device.flushes, flushes_before + 2);
        let loaded = load_allocator(&mut device, &superblock).unwrap();
        assert!(!loaded.is_owned(first).unwrap());
        assert!(loaded.is_owned(second).unwrap());
        assert_eq!(loaded.allocated_blocks(), 1);
        loaded.validate().unwrap();
    }

    #[test]
    fn crash_before_commit_does_not_mutate_allocation_home_state() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, superblock.allocation_start, [0xa5; BLOCK_SIZE])
            .unwrap();
        store_journal_image(&mut device, superblock, log.entries()).unwrap();

        let report = recover_journal(&mut device, superblock).unwrap();

        assert_eq!(report, RecoveryReport::default());
        let loaded = load_allocator(&mut device, &superblock).unwrap();
        assert_eq!(loaded.allocated_blocks(), 0);
        loaded.validate().unwrap();
    }

    #[test]
    fn committed_allocator_update_survives_home_write_failure_and_replays_idempotently() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let mut allocator = load_allocator(&mut device, &superblock).unwrap();
        let owned = allocator.allocate().unwrap();
        device.fail_once_on = Some(superblock.allocation_start);

        assert_eq!(
            store_allocator_journaled(&mut device, &superblock, &allocator)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );

        let report = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 1);
        let loaded = load_allocator(&mut device, &superblock).unwrap();
        assert!(loaded.is_owned(owned).unwrap());
        assert_eq!(loaded.allocated_blocks(), 1);
        loaded.validate().unwrap();

        let second_replay = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(second_replay, report);
        let loaded_again = load_allocator(&mut device, &superblock).unwrap();
        assert!(loaded_again.is_owned(owned).unwrap());
        loaded_again.validate().unwrap();
    }

    #[test]
    fn whole_allocator_transaction_rejects_insufficient_journal_capacity() {
        let mut device = FaultDevice::new(33_000);
        let superblock = Superblock::with_journal_blocks(device.block_count(), 2).unwrap();
        crate::allocation_disk::initialize_allocation_region(&mut device, &superblock).unwrap();
        assert_eq!(superblock.allocation_blocks, 2);
        let allocator = load_allocator(&mut device, &superblock).unwrap();

        assert_eq!(
            store_allocator_journaled(&mut device, &superblock, &allocator)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
