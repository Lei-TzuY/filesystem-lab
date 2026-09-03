use std::collections::BTreeMap;
use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::store_directory_table;
use crate::format::Superblock;
use crate::journal::JournalLog;
use crate::journal_region::store_journal_image;
use crate::recovery::{recover_journal, RecoveryReport};

/// Persists one directory-table snapshot through the bounded write-ahead log.
///
/// The desired table is first rendered into an isolated capture device using the normal
/// directory-table encoder. Only home blocks whose rendered contents differ from the current
/// durable directory region are included in one journal transaction. The journal is flushed before
/// committed recovery writes those blocks home and crosses the home-location flush boundary.
///
/// This remains deliberately bounded: if all changed directory-table blocks plus transaction
/// framing do not fit the fixed journal reservation, the update fails rather than being split across
/// commits. An already-identical snapshot is a no-op and does not rewrite the journal.
///
/// # Errors
///
/// Returns `InvalidInput` when device geometry disagrees with the superblock or the bounded journal
/// cannot contain every changed directory-table block. Encoding, journal, home-write, and flush
/// failures propagate. A returned error never claims that the requested directory-table state is
/// durable.
pub fn store_directory_table_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    entries: &[PersistedDirectoryEntry],
) -> io::Result<RecoveryReport> {
    if device.block_count() != superblock.total_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "journaled directory-table device geometry does not match superblock",
        ));
    }

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_directory_table(&mut capture, superblock, entries)?;

    let mut changed = Vec::new();
    for block in superblock.directory_range() {
        let desired = capture.blocks.remove(&block).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "directory-table image did not render every directory metadata block",
            )
        })?;
        let mut current = [0_u8; BLOCK_SIZE];
        device.read_block(block, &mut current)?;
        if current != desired {
            changed.push((block, desired));
        }
    }
    if !capture.blocks.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory-table image rendered outside directory metadata region",
        ));
    }
    if changed.is_empty() {
        return Ok(RecoveryReport::default());
    }

    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for (block, data) in changed.iter().copied() {
        log.write(txid, block, data)?;
    }
    log.commit(txid)?;

    store_journal_image(device, *superblock, log.entries())?;
    let report = recover_journal(device, *superblock)?;
    if report.committed_transactions != 1 || report.home_writes != changed.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "journaled directory-table recovery report does not match one complete transaction",
        ));
    }
    Ok(report)
}

#[derive(Debug)]
struct CaptureDevice {
    block_count: u64,
    blocks: BTreeMap<u64, [u8; BLOCK_SIZE]>,
}

impl CaptureDevice {
    fn new(block_count: u64) -> Self {
        Self {
            block_count,
            blocks: BTreeMap::new(),
        }
    }
}

impl BlockDevice for CaptureDevice {
    fn block_count(&self) -> u64 {
        self.block_count
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        if block >= self.block_count {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "capture-device read is out of range",
            ));
        }
        if let Some(data) = self.blocks.get(&block) {
            *buf = *data;
        } else {
            buf.fill(0);
        }
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        if block >= self.block_count {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "capture-device write is out of range",
            ));
        }
        self.blocks.insert(block, *buf);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::directory_table::load_directory_table;
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
                return Err(io::Error::other("injected home directory-table write failure"));
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

    fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
        PersistedDirectoryEntry {
            parent,
            target,
            name: name.to_owned(),
        }
    }

    #[test]
    fn journaled_directory_update_crosses_log_then_home_durability_boundaries() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let desired = entry(1, 2, "child");
        let flushes_before = device.flushes;

        let report = store_directory_table_journaled(
            &mut device,
            &superblock,
            std::slice::from_ref(&desired),
        )
        .unwrap();

        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 1);
        assert_eq!(device.flushes, flushes_before + 2);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            vec![desired]
        );
    }

    #[test]
    fn identical_directory_snapshot_is_a_noop() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let desired = entry(1, 2, "child");
        store_directory_table_journaled(
            &mut device,
            &superblock,
            std::slice::from_ref(&desired),
        )
        .unwrap();
        let flushes_before = device.flushes;

        let report = store_directory_table_journaled(&mut device, &superblock, &[desired]).unwrap();

        assert_eq!(report, RecoveryReport::default());
        assert_eq!(device.flushes, flushes_before);
    }

    #[test]
    fn crash_before_commit_does_not_mutate_directory_home_state() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let original = load_directory_table(&mut device, &superblock).unwrap();
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, superblock.directory_start, [0xa5; BLOCK_SIZE])
            .unwrap();
        store_journal_image(&mut device, superblock, log.entries()).unwrap();

        let report = recover_journal(&mut device, superblock).unwrap();

        assert_eq!(report, RecoveryReport::default());
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            original
        );
    }

    #[test]
    fn committed_directory_update_survives_home_write_failure_and_replays_idempotently() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let desired = entry(1, 2, "child");
        device.fail_once_on = Some(superblock.directory_start);

        assert_eq!(
            store_directory_table_journaled(
                &mut device,
                &superblock,
                std::slice::from_ref(&desired),
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other
        );

        let report = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 1);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            vec![desired.clone()]
        );

        let second_replay = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(second_replay, report);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            vec![desired]
        );
    }

    #[test]
    fn multi_block_directory_change_rejects_insufficient_journal_capacity() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let entries: Vec<_> = (0..100)
            .map(|index| {
                entry(
                    1,
                    u64::try_from(index + 2).unwrap(),
                    &format!("entry-{index:03}-{}", "x".repeat(32)),
                )
            })
            .collect();

        assert_eq!(
            store_directory_table_journaled(&mut device, &superblock, &entries)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
