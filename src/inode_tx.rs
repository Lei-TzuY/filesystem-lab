use std::collections::BTreeMap;
use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::Superblock;
use crate::inode_codec::PersistedInode;
use crate::inode_table::store_inode_table;
use crate::journal::JournalLog;
use crate::journal_region::store_journal_image;
use crate::recovery::{recover_journal, RecoveryReport};

/// Persists one inode-table snapshot through the bounded write-ahead log.
///
/// The desired table is first rendered into an isolated capture device using the normal inode-table
/// encoder. Only home blocks whose rendered contents differ from the current durable inode region
/// are included in one journal transaction. The journal is flushed before committed recovery writes
/// those blocks home and crosses the home-location flush boundary.
///
/// This remains deliberately bounded: if all changed inode-table blocks plus transaction framing do
/// not fit the fixed journal reservation, the update fails rather than being split across commits.
/// An already-identical snapshot is a no-op and does not rewrite the journal.
///
/// # Errors
///
/// Returns `InvalidInput` when device geometry disagrees with the superblock or the bounded journal
/// cannot contain every changed inode-table block. Encoding, journal, home-write, and flush failures
/// propagate. A returned error never claims that the requested inode-table state is durable.
pub fn store_inode_table_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inodes: &[PersistedInode],
) -> io::Result<RecoveryReport> {
    if device.block_count() != superblock.total_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "journaled inode-table device geometry does not match superblock",
        ));
    }

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_inode_table(&mut capture, superblock, inodes)?;

    let mut changed = Vec::new();
    for block in superblock.inode_range() {
        let desired = capture.blocks.remove(&block).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "inode-table image did not render every inode metadata block",
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
            "inode-table image rendered outside inode metadata region",
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
            "journaled inode-table recovery report does not match one complete transaction",
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
    use crate::format::format_device;
    use crate::inode::InodeKind;
    use crate::inode_codec::PersistedInode;
    use crate::inode_table::load_inode_table;
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
                return Err(io::Error::other("injected home inode-table write failure"));
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

    fn file_inode(id: u64, block: u64) -> PersistedInode {
        PersistedInode {
            id,
            kind: InodeKind::File,
            blocks: vec![block],
        }
    }

    #[test]
    fn journaled_inode_update_crosses_log_then_home_durability_boundaries() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let inode = file_inode(1, superblock.reserved_blocks());
        let flushes_before = device.flushes;

        let report =
            store_inode_table_journaled(&mut device, &superblock, std::slice::from_ref(&inode))
                .unwrap();

        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 1);
        assert_eq!(device.flushes, flushes_before + 2);
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap(),
            vec![inode]
        );
    }

    #[test]
    fn identical_inode_snapshot_is_a_noop() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let inode = file_inode(1, superblock.reserved_blocks());
        store_inode_table_journaled(&mut device, &superblock, std::slice::from_ref(&inode))
            .unwrap();
        let flushes_before = device.flushes;

        let report = store_inode_table_journaled(&mut device, &superblock, &[inode]).unwrap();

        assert_eq!(report, RecoveryReport::default());
        assert_eq!(device.flushes, flushes_before);
    }

    #[test]
    fn crash_before_commit_does_not_mutate_inode_home_state() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let original = load_inode_table(&mut device, &superblock).unwrap();
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, superblock.inode_start, [0xa5; BLOCK_SIZE])
            .unwrap();
        store_journal_image(&mut device, superblock, log.entries()).unwrap();

        let report = recover_journal(&mut device, superblock).unwrap();

        assert_eq!(report, RecoveryReport::default());
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap(),
            original
        );
    }

    #[test]
    fn committed_inode_update_survives_home_write_failure_and_replays_idempotently() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let inode = file_inode(1, superblock.reserved_blocks());
        device.fail_once_on = Some(superblock.inode_start);

        assert_eq!(
            store_inode_table_journaled(&mut device, &superblock, std::slice::from_ref(&inode),)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );

        let report = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 1);
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap(),
            vec![inode.clone()]
        );

        let second_replay = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(second_replay, report);
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap(),
            vec![inode]
        );
    }

    #[test]
    fn multi_block_inode_change_rejects_insufficient_journal_capacity() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        let inodes: Vec<_> = (1..=200)
            .map(|id| PersistedInode {
                id,
                kind: InodeKind::File,
                blocks: Vec::new(),
            })
            .collect();

        assert_eq!(
            store_inode_table_journaled(&mut device, &superblock, &inodes)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
