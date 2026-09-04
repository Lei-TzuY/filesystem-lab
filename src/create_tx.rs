use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::store_allocator;
use crate::block::BlockDevice;
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::store_directory_table;
use crate::format::Superblock;
use crate::inode_codec::PersistedInode;
use crate::inode_table::store_inode_table;
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

/// Persists allocation, inode, and directory snapshots in one bounded WAL transaction.
///
/// This primitive is intended for lifecycle changes such as creating a reachable file whose inode
/// immediately owns newly allocated data blocks. All three desired metadata images are rendered in
/// isolation first, only changed home blocks are logged, and no home location is written until the
/// complete transaction has crossed the journal durability boundary. After all committed home
/// writes are durable, the fixed journal reservation is checkpointed before successful return so a
/// later transaction can reuse it immediately.
///
/// The transaction is never split. If the reserved journal cannot hold the complete changed-block
/// set plus transaction framing, the operation fails before publishing a new journal image.
///
/// # Errors
///
/// Returns `InvalidInput` when device or allocator geometry disagrees with the superblock, or when
/// the bounded journal cannot contain the complete transaction. Encoding, journal, recovery,
/// home-write, checkpoint, and flush failures are propagated. A failure may follow a durable
/// commit; callers must run recovery and checkpointing before interpreting the home metadata as a
/// consistent completed state.
pub fn store_create_metadata_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    allocator: &BlockAllocator,
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
) -> io::Result<RecoveryReport> {
    if device.block_count() != superblock.total_blocks {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic-create device geometry does not match superblock",
        ));
    }

    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, allocator)?;
    store_inode_table(&mut capture, superblock, inodes)?;
    store_directory_table(&mut capture, superblock, entries)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "atomic-create image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "atomic-create image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.directory_range(),
        "atomic-create image did not render every directory metadata block",
        &mut changed,
    )?;
    capture.ensure_empty(
        "atomic-create image rendered outside allocation, inode, and directory regions",
    )?;
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
    let report = recover_journal_and_checkpoint(device, *superblock)?;
    if report.committed_transactions != 1 || report.home_writes != changed.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "atomic-create recovery report does not match one complete transaction",
        ));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocation_disk::{initialize_allocation_region, load_allocator};
    use crate::block::BLOCK_SIZE;
    use crate::directory_table::{initialize_directory_table_region, load_directory_table};
    use crate::format::{Superblock, SUPERBLOCK_BLOCK};
    use crate::fsck::check_device;
    use crate::inode::InodeKind;
    use crate::inode_table::{initialize_inode_table_region, load_inode_table};
    use crate::journal_region::load_journal_image;
    use crate::recovery::recover_journal;

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
                    "injected atomic-create home-write failure",
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

    fn desired_create(
        device: &mut FaultDevice,
        superblock: &Superblock,
    ) -> (
        BlockAllocator,
        u64,
        Vec<PersistedInode>,
        Vec<PersistedDirectoryEntry>,
    ) {
        let mut allocator = load_allocator(device, superblock).unwrap();
        let data_block = allocator.allocate().unwrap();
        let inodes = vec![
            PersistedInode {
                id: 1,
                kind: InodeKind::Directory,
                blocks: Vec::new(),
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::File,
                blocks: vec![data_block],
            },
        ];
        let entries = vec![PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "child".to_owned(),
        }];
        (allocator, data_block, inodes, entries)
    }

    #[test]
    fn atomic_create_commits_ownership_inode_and_namespace_together() {
        let mut device = FaultDevice::new(64);
        let superblock = format_with_journal(&mut device, 4);
        let (allocator, data_block, inodes, entries) = desired_create(&mut device, &superblock);
        let flushes_before = device.flushes;

        let report = store_create_metadata_journaled(
            &mut device,
            &superblock,
            &allocator,
            &inodes,
            &entries,
        )
        .unwrap();

        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 3);
        assert_eq!(device.flushes, flushes_before + 3);
        assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
        assert!(load_allocator(&mut device, &superblock)
            .unwrap()
            .is_owned(data_block)
            .unwrap());
        assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            entries
        );
        check_device(&mut device).unwrap();
    }

    #[test]
    fn committed_create_recovers_after_namespace_home_write_fails() {
        let mut device = FaultDevice::new(64);
        let superblock = format_with_journal(&mut device, 4);
        let (allocator, data_block, inodes, entries) = desired_create(&mut device, &superblock);
        device.fail_once_on = Some(superblock.directory_start);

        assert_eq!(
            store_create_metadata_journaled(
                &mut device,
                &superblock,
                &allocator,
                &inodes,
                &entries,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other
        );

        assert!(load_allocator(&mut device, &superblock)
            .unwrap()
            .is_owned(data_block)
            .unwrap());
        assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
        assert!(load_directory_table(&mut device, &superblock)
            .unwrap()
            .is_empty());
        assert!(check_device(&mut device).is_err());

        let report = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 3);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            entries
        );
        check_device(&mut device).unwrap();

        let replay = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(replay, report);
        check_device(&mut device).unwrap();
    }

    #[test]
    fn default_three_block_journal_rejects_three_home_block_create_atomically() {
        let mut device = FaultDevice::new(64);
        let superblock = format_with_journal(&mut device, 3);
        let (allocator, data_block, inodes, entries) = desired_create(&mut device, &superblock);
        let flushes_before = device.flushes;

        assert_eq!(
            store_create_metadata_journaled(
                &mut device,
                &superblock,
                &allocator,
                &inodes,
                &entries,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidInput
        );

        assert_eq!(device.flushes, flushes_before);
        assert!(!load_allocator(&mut device, &superblock)
            .unwrap()
            .is_owned(data_block)
            .unwrap());
        assert!(load_inode_table(&mut device, &superblock)
            .unwrap()
            .is_empty());
        assert!(load_directory_table(&mut device, &superblock)
            .unwrap()
            .is_empty());
    }
}
