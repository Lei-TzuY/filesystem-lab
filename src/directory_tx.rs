use std::io;

use crate::block::BlockDevice;
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::{load_directory_table, store_directory_table};
use crate::format::Superblock;
use crate::journal::JournalLog;
use crate::journal_region::{
    append_retained_journal_entries, load_journal_image, load_retained_journal_entries,
    store_journal_image,
};
use crate::recovery::{plan_recovery, recover_journal, RecoveryReport};
use crate::recovery_projection::{
    check_device_after_entries_projection, projected_device_after_entries,
};
use crate::transaction_image::CaptureDevice;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetainedDirectoryUpdateReport {
    pub retained: RecoveryReport,
    pub appended_home_writes: usize,
}

/// Applies one directory-table mutation against the logical state produced by any retained WAL and
/// appends the resulting complete transaction to journal-region v3 without replaying it home.
///
/// The update closure observes the directory table after all currently retained committed
/// transactions are projected. The desired snapshot is rendered and diffed against that projected
/// state, not stale home blocks. Before publication, the complete retained stream plus the new
/// transaction is projected through strict fsck. A semantically invalid candidate therefore fails
/// before any journal write or flush.
///
/// This is the first high-level transaction path that can build dependent retained mutations: a
/// second call can observe namespace changes retained by the first call even though home metadata has
/// not yet been replayed.
///
/// # Errors
///
/// Returns `WouldBlock` when a non-empty v1/v2 journal is active. Propagates retained-journal
/// decode/capacity errors, directory encoding errors, projected strict-fsck failures, closure errors,
/// and block-device failures. A no-op closure produces no journal mutation.
pub fn update_directory_table_retained_journaled<F>(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    update: F,
) -> io::Result<RetainedDirectoryUpdateReport>
where
    F: FnOnce(&mut Vec<PersistedDirectoryEntry>) -> io::Result<()>,
{
    if device.block_count() != superblock.total_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "retained directory-table device geometry does not match superblock",
        ));
    }

    let current_entries = if let Some(entries) = load_retained_journal_entries(device, *superblock)?
    {
        entries
    } else {
        let existing = load_journal_image(device, *superblock)?;
        if !existing.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "active v1/v2 journal must be checkpointed before retained directory update",
            ));
        }
        Vec::new()
    };

    let (mut projected, current_report) = projected_device_after_entries(device, &current_entries)?;
    let mut desired_entries = load_directory_table(&mut projected, superblock)?;
    update(&mut desired_entries)?;

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_directory_table(&mut capture, superblock, &desired_entries)?;
    let mut changed = Vec::new();
    capture.collect_changed_range(
        &mut projected,
        superblock.directory_range(),
        "retained directory image did not render every directory metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("retained directory image rendered outside directory metadata region")?;
    drop(projected);

    if changed.is_empty() {
        return Ok(RetainedDirectoryUpdateReport {
            retained: current_report,
            appended_home_writes: 0,
        });
    }

    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for (block, data) in changed.iter().copied() {
        log.write(txid, block, data)?;
    }
    log.commit(txid)?;

    let mut candidate = current_entries;
    candidate.extend_from_slice(log.entries());
    let projected_report = check_device_after_entries_projection(device, &candidate)?;
    let candidate_plan = plan_recovery(&candidate)?;
    if projected_report.recovery != candidate_plan.report() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained directory projection disagrees with recovery plan",
        ));
    }

    append_retained_journal_entries(device, *superblock, log.entries())?;
    Ok(RetainedDirectoryUpdateReport {
        retained: projected_report.recovery,
        appended_home_writes: changed.len(),
    })
}

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
    capture.collect_changed_range(
        device,
        superblock.directory_range(),
        "directory-table image did not render every directory metadata block",
        &mut changed,
    )?;
    capture.ensure_empty("directory-table image rendered outside directory metadata region")?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BLOCK_SIZE;
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
                return Err(io::Error::other(
                    "injected home directory-table write failure",
                ));
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
        // v2 publication crosses empty-anchor, staged-tail, and active-anchor barriers before
        // replay crosses the home durability boundary.
        assert_eq!(device.flushes, flushes_before + 4);
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
        store_directory_table_journaled(&mut device, &superblock, std::slice::from_ref(&desired))
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
