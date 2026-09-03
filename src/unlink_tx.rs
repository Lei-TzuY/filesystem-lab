use std::io;

use crate::allocation::BlockAllocator;
use crate::block::BlockDevice;
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_codec::PersistedDirectoryEntry;
use crate::format::Superblock;
use crate::inode_codec::PersistedInode;
use crate::recovery::RecoveryReport;

/// Persists an unlink lifecycle as one bounded allocation/inode/directory WAL transaction.
///
/// Callers provide the complete desired post-unlink snapshots: the target directory entry removed,
/// the unlinked inode removed when its lifecycle ends, and any blocks that inode owned released in
/// the allocator. The three metadata images cross the same journal durability boundary, so recovery
/// can finish a committed unlink after a crash between home writes.
///
/// This primitive intentionally reuses the same three-table transaction engine as atomic create;
/// create and unlink differ in the desired snapshots, not in WAL ordering or recovery semantics.
/// The transaction is never split when the bounded journal is too small.
///
/// # Errors
///
/// Propagates geometry, encoding, journal-capacity, journal-write, recovery, home-write, and flush
/// failures from the shared three-table transaction engine. A home-write failure may occur after a
/// durable commit; callers must run journal recovery before interpreting home metadata.
pub fn store_unlink_metadata_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    allocator: &BlockAllocator,
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
) -> io::Result<RecoveryReport> {
    store_create_metadata_journaled(device, superblock, allocator, inodes, entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocation_disk::{initialize_allocation_region, load_allocator};
    use crate::block::BLOCK_SIZE;
    use crate::create_tx::store_create_metadata_journaled;
    use crate::directory_table::{initialize_directory_table_region, load_directory_table};
    use crate::format::{Superblock, SUPERBLOCK_BLOCK};
    use crate::fsck::check_device;
    use crate::inode::InodeKind;
    use crate::inode_table::{initialize_inode_table_region, load_inode_table};
    use crate::recovery::recover_journal;

    #[derive(Debug)]
    struct FaultDevice {
        blocks: Vec<[u8; BLOCK_SIZE]>,
        fail_once_on: Option<u64>,
    }

    impl FaultDevice {
        fn new(blocks: usize) -> Self {
            Self {
                blocks: vec![[0_u8; BLOCK_SIZE]; blocks],
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
                return Err(io::Error::other("injected atomic-unlink home-write failure"));
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
            Ok(())
        }
    }

    fn format_device(device: &mut FaultDevice) -> Superblock {
        let superblock = Superblock::with_journal_blocks(device.block_count(), 4).unwrap();
        initialize_allocation_region(device, &superblock).unwrap();
        initialize_inode_table_region(device, &superblock).unwrap();
        initialize_directory_table_region(device, &superblock).unwrap();
        device
            .write_block(SUPERBLOCK_BLOCK, &superblock.encode())
            .unwrap();
        device.flush().unwrap();
        superblock
    }

    fn seed_linked_file(
        device: &mut FaultDevice,
        superblock: &Superblock,
    ) -> (u64, Vec<PersistedInode>, Vec<PersistedDirectoryEntry>) {
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
        store_create_metadata_journaled(device, superblock, &allocator, &inodes, &entries).unwrap();
        check_device(device).unwrap();
        (data_block, inodes, entries)
    }

    #[test]
    fn atomic_unlink_removes_namespace_inode_and_block_ownership_together() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device);
        let (data_block, _, _) = seed_linked_file(&mut device, &superblock);
        let mut allocator = load_allocator(&mut device, &superblock).unwrap();
        allocator.free(data_block).unwrap();
        let remaining_inodes = vec![PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        }];

        let report = store_unlink_metadata_journaled(
            &mut device,
            &superblock,
            &allocator,
            &remaining_inodes,
            &[],
        )
        .unwrap();

        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 3);
        assert!(!load_allocator(&mut device, &superblock)
            .unwrap()
            .is_owned(data_block)
            .unwrap());
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap(),
            remaining_inodes
        );
        assert!(load_directory_table(&mut device, &superblock)
            .unwrap()
            .is_empty());
        check_device(&mut device).unwrap();
    }

    #[test]
    fn committed_unlink_recovers_after_inode_home_write_fails() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device);
        let (data_block, original_inodes, original_entries) =
            seed_linked_file(&mut device, &superblock);
        let mut allocator = load_allocator(&mut device, &superblock).unwrap();
        allocator.free(data_block).unwrap();
        let remaining_inodes = vec![PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        }];
        device.fail_once_on = Some(superblock.inode_start);

        assert_eq!(
            store_unlink_metadata_journaled(
                &mut device,
                &superblock,
                &allocator,
                &remaining_inodes,
                &[],
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other
        );

        assert!(!load_allocator(&mut device, &superblock)
            .unwrap()
            .is_owned(data_block)
            .unwrap());
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap(),
            original_inodes
        );
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            original_entries
        );
        assert!(check_device(&mut device).is_err());

        let report = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 3);
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap(),
            remaining_inodes
        );
        assert!(load_directory_table(&mut device, &superblock)
            .unwrap()
            .is_empty());
        check_device(&mut device).unwrap();

        let replay = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(replay, report);
        check_device(&mut device).unwrap();
    }
}
