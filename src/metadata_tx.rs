use std::collections::BTreeMap;
use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::store_directory_table;
use crate::format::Superblock;
use crate::inode_codec::PersistedInode;
use crate::inode_table::store_inode_table;
use crate::journal::JournalLog;
use crate::journal_region::store_journal_image;
use crate::recovery::{recover_journal, RecoveryReport};

/// Persists inode-table and directory-table snapshots in one bounded WAL transaction.
///
/// Both desired metadata tables are rendered into one isolated capture device first. Only home
/// blocks whose rendered contents differ from the current durable inode or directory regions are
/// added to the transaction. The complete cross-table write set is committed to the journal before
/// recovery writes any home block, so a committed transaction can be replayed after a crash between
/// inode and directory home writes.
///
/// The transaction is deliberately bounded by the existing journal reservation. If the combined
/// changed-block set plus transaction framing does not fit, the operation fails before publishing a
/// new journal image rather than splitting one logical namespace change across commits. An already
/// identical pair of snapshots is a no-op.
///
/// # Errors
///
/// Returns `InvalidInput` when device geometry disagrees with the superblock or the bounded journal
/// cannot contain the complete combined write set. Table encoding, journal, home-write, and flush
/// failures propagate. A returned home-write error may follow a durable commit; callers must run
/// journal recovery before treating the on-device home locations as a consistent snapshot.
pub fn store_inode_directory_tables_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
) -> io::Result<RecoveryReport> {
    if device.block_count() != superblock.total_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "journaled metadata device geometry does not match superblock",
        ));
    }

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_inode_table(&mut capture, superblock, inodes)?;
    store_directory_table(&mut capture, superblock, entries)?;

    let mut changed = Vec::new();
    collect_region_changes(device, &mut capture, superblock.inode_range(), &mut changed)?;
    collect_region_changes(
        device,
        &mut capture,
        superblock.directory_range(),
        &mut changed,
    )?;
    if !capture.blocks.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "combined metadata image rendered outside inode and directory regions",
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
            "combined metadata recovery report does not match one complete transaction",
        ));
    }
    Ok(report)
}

fn collect_region_changes(
    device: &mut impl BlockDevice,
    capture: &mut CaptureDevice,
    range: std::ops::Range<u64>,
    changed: &mut Vec<(u64, [u8; BLOCK_SIZE])>,
) -> io::Result<()> {
    for block in range {
        let desired = capture.blocks.remove(&block).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "combined metadata image did not render every metadata block",
            )
        })?;
        let mut current = [0_u8; BLOCK_SIZE];
        device.read_block(block, &mut current)?;
        if current != desired {
            changed.push((block, desired));
        }
    }
    Ok(())
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
    use crate::allocation_disk::initialize_allocation_region;
    use crate::directory_table::{initialize_directory_table_region, load_directory_table};
    use crate::format::{Superblock, SUPERBLOCK_BLOCK};
    use crate::fsck::check_device;
    use crate::inode::InodeKind;
    use crate::inode_table::{initialize_inode_table_region, load_inode_table};
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
                    "injected combined metadata home-write failure",
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

    fn format_with_journal(device: &mut FaultDevice, journal_blocks: u64) -> Superblock {
        let superblock =
            Superblock::with_journal_blocks(device.block_count(), journal_blocks).unwrap();
        initialize_allocation_region(device, &superblock).unwrap();
        initialize_inode_table_region(device, &superblock).unwrap();
        initialize_directory_table_region(device, &superblock).unwrap();
        device
            .write_block(SUPERBLOCK_BLOCK, &superblock.encode())
            .unwrap();
        device.flush().unwrap();
        superblock
    }

    fn desired_metadata() -> (Vec<PersistedInode>, Vec<PersistedDirectoryEntry>) {
        let inodes = vec![
            PersistedInode {
                id: 1,
                kind: InodeKind::Directory,
                blocks: Vec::new(),
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::File,
                blocks: Vec::new(),
            },
        ];
        let entries = vec![PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "child".to_owned(),
        }];
        (inodes, entries)
    }

    #[test]
    fn combined_metadata_update_commits_inode_and_namespace_in_one_transaction() {
        let mut device = FaultDevice::new(64);
        let superblock = format_with_journal(&mut device, 3);
        let (inodes, entries) = desired_metadata();
        let flushes_before = device.flushes;

        let report =
            store_inode_directory_tables_journaled(&mut device, &superblock, &inodes, &entries)
                .unwrap();

        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 2);
        assert_eq!(device.flushes, flushes_before + 2);
        assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            entries
        );
        let fsck = check_device(&mut device).unwrap();
        assert_eq!(fsck.inode_records, 2);
        assert_eq!(fsck.directory_entries, 1);
    }

    #[test]
    fn identical_combined_snapshot_is_a_noop() {
        let mut device = FaultDevice::new(64);
        let superblock = format_with_journal(&mut device, 3);
        let (inodes, entries) = desired_metadata();
        store_inode_directory_tables_journaled(&mut device, &superblock, &inodes, &entries)
            .unwrap();
        let flushes_before = device.flushes;

        let report =
            store_inode_directory_tables_journaled(&mut device, &superblock, &inodes, &entries)
                .unwrap();

        assert_eq!(report, RecoveryReport::default());
        assert_eq!(device.flushes, flushes_before);
    }

    #[test]
    fn crash_before_combined_commit_mutates_neither_home_table() {
        let mut device = FaultDevice::new(64);
        let superblock = format_with_journal(&mut device, 3);
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, superblock.inode_start, [0xa5; BLOCK_SIZE])
            .unwrap();
        log.write(txid, superblock.directory_start, [0x5a; BLOCK_SIZE])
            .unwrap();
        store_journal_image(&mut device, superblock, log.entries()).unwrap();

        let report = recover_journal(&mut device, superblock).unwrap();

        assert_eq!(report, RecoveryReport::default());
        assert!(load_inode_table(&mut device, &superblock)
            .unwrap()
            .is_empty());
        assert!(load_directory_table(&mut device, &superblock)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn committed_combined_update_recovers_after_second_home_write_fails() {
        let mut device = FaultDevice::new(64);
        let superblock = format_with_journal(&mut device, 3);
        let (inodes, entries) = desired_metadata();
        device.fail_once_on = Some(superblock.directory_start);

        assert_eq!(
            store_inode_directory_tables_journaled(&mut device, &superblock, &inodes, &entries,)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );

        assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
        assert!(load_directory_table(&mut device, &superblock)
            .unwrap()
            .is_empty());

        let report = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 2);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            entries
        );
        check_device(&mut device).unwrap();

        let second_replay = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(second_replay, report);
        check_device(&mut device).unwrap();
    }

    #[test]
    fn combined_update_rejects_a_journal_too_small_for_both_home_blocks() {
        let mut device = FaultDevice::new(64);
        let superblock = format_with_journal(&mut device, 2);
        let (inodes, entries) = desired_metadata();
        let flushes_before = device.flushes;

        assert_eq!(
            store_inode_directory_tables_journaled(&mut device, &superblock, &inodes, &entries,)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );

        assert_eq!(device.flushes, flushes_before);
        assert!(load_inode_table(&mut device, &superblock)
            .unwrap()
            .is_empty());
        assert!(load_directory_table(&mut device, &superblock)
            .unwrap()
            .is_empty());
    }
}
