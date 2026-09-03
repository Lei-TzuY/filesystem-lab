use std::collections::{BTreeMap, BTreeSet};
use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::load_inode_table;
use crate::recovery::RecoveryReport;

/// Persists one validated unlink lifecycle as a bounded allocation/inode/directory WAL transaction.
///
/// The desired snapshots must describe exactly one unlink transition from the currently durable
/// home metadata: one namespace entry disappears, its target inode disappears, and exactly the
/// blocks owned by that inode become free. Existing inodes and namespace entries may not otherwise
/// change. Directories may be removed only when empty, the root inode may not be removed, and an
/// inode with another durable namespace reference may not be removed by this primitive.
///
/// Validation happens before a new journal image is published. This keeps caller mistakes from
/// becoming crash-consistent corruption merely because the three home regions advance atomically.
/// The transaction is never split when the bounded journal is too small.
///
/// # Errors
///
/// Returns `InvalidInput` when the desired snapshots are not exactly one legal unlink lifecycle.
/// Propagates durable metadata loading, geometry, encoding, journal-capacity, journal-write,
/// recovery, home-write, and flush failures. A home-write failure may occur after a durable commit;
/// callers must run journal recovery before interpreting home metadata.
pub fn store_unlink_metadata_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    allocator: &BlockAllocator,
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
) -> io::Result<RecoveryReport> {
    validate_unlink_transition(device, superblock, allocator, inodes, entries)?;
    store_create_metadata_journaled(device, superblock, allocator, inodes, entries)
}

/// Validates that desired metadata is exactly one legal unlink transition from durable home state.
///
/// This is deliberately narrower than general POSIX unlink semantics. Hard links, orphan handling,
/// recursive directory removal, and rename are not modeled yet, so this validator rejects any
/// transition that would require those semantics.
///
/// # Errors
///
/// Returns `InvalidInput` when allocator geometry changes, when the inode or namespace delta is not
/// exactly one unlink, or when the allocator delta does not free exactly the removed inode's blocks.
/// Durable allocation, inode-table, and directory-table decoding failures are propagated.
#[allow(clippy::too_many_lines)]
pub fn validate_unlink_transition(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    desired_allocator: &BlockAllocator,
    desired_inodes: &[PersistedInode],
    desired_entries: &[PersistedDirectoryEntry],
) -> io::Result<()> {
    let current_allocator = load_allocator(device, superblock)?;
    let current_inodes = load_inode_table(device, superblock)?;
    let current_entries = load_directory_table(device, superblock)?;

    if current_allocator.total_blocks() != desired_allocator.total_blocks()
        || current_allocator.reserved_blocks() != desired_allocator.reserved_blocks()
    {
        return invalid("unlink allocator geometry changed");
    }

    let current_by_id: BTreeMap<u64, &PersistedInode> = current_inodes
        .iter()
        .map(|inode| (inode.id, inode))
        .collect();
    let desired_by_id: BTreeMap<u64, &PersistedInode> = desired_inodes
        .iter()
        .map(|inode| (inode.id, inode))
        .collect();
    if current_by_id.len() != current_inodes.len() || desired_by_id.len() != desired_inodes.len() {
        return invalid("unlink inode snapshot contains duplicate identifiers");
    }

    let removed_ids: Vec<u64> = current_by_id
        .keys()
        .filter(|id| !desired_by_id.contains_key(id))
        .copied()
        .collect();
    if removed_ids.len() != 1 {
        return invalid("unlink must remove exactly one inode");
    }
    if desired_by_id
        .keys()
        .any(|id| !current_by_id.contains_key(id))
    {
        return invalid("unlink may not add an inode");
    }
    for (id, current) in &current_by_id {
        if let Some(desired) = desired_by_id.get(id) {
            if *current != *desired {
                return invalid("unlink may not modify surviving inodes");
            }
        }
    }

    let removed_id = removed_ids[0];
    if removed_id == 1 {
        return invalid("unlink may not remove the root inode");
    }
    let removed_inode = current_by_id[&removed_id];

    let current_entry_set: BTreeSet<(u64, String, u64)> = current_entries
        .iter()
        .map(|entry| (entry.parent, entry.name.clone(), entry.target))
        .collect();
    let desired_entry_set: BTreeSet<(u64, String, u64)> = desired_entries
        .iter()
        .map(|entry| (entry.parent, entry.name.clone(), entry.target))
        .collect();
    if current_entry_set.len() != current_entries.len()
        || desired_entry_set.len() != desired_entries.len()
    {
        return invalid("unlink namespace snapshot contains duplicate entries");
    }
    if desired_entry_set
        .iter()
        .any(|entry| !current_entry_set.contains(entry))
    {
        return invalid("unlink may not add or retarget a namespace entry");
    }
    let removed_entries: Vec<&(u64, String, u64)> = current_entry_set
        .iter()
        .filter(|entry| !desired_entry_set.contains(*entry))
        .collect();
    if removed_entries.len() != 1 {
        return invalid("unlink must remove exactly one namespace entry");
    }
    if removed_entries[0].2 != removed_id {
        return invalid("removed namespace entry must target the removed inode");
    }
    if current_entries
        .iter()
        .filter(|entry| entry.target == removed_id)
        .count()
        != 1
    {
        return invalid("unlink cannot remove an inode with another namespace reference");
    }
    if desired_entries
        .iter()
        .any(|entry| entry.target == removed_id || entry.parent == removed_id)
    {
        return invalid("unlink leaves a namespace reference to the removed inode");
    }
    if removed_inode.kind == InodeKind::Directory
        && current_entries
            .iter()
            .any(|entry| entry.parent == removed_id)
    {
        return invalid("unlink cannot remove a non-empty directory");
    }

    let expected_freed: BTreeSet<u64> = removed_inode.blocks.iter().copied().collect();
    let mut observed_freed = BTreeSet::new();
    for block in current_allocator.reserved_blocks()..current_allocator.total_blocks() {
        let current_owned = current_allocator
            .is_owned(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let desired_owned = desired_allocator
            .is_owned(block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        match (current_owned, desired_owned) {
            (true, false) => {
                observed_freed.insert(block);
            }
            (false, true) => return invalid("unlink may not allocate a data block"),
            _ => {}
        }
    }
    if observed_freed != expected_freed {
        return invalid("unlink must free exactly the removed inode's data blocks");
    }

    Ok(())
}

fn invalid(message: &'static str) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::InvalidInput, message))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocation_disk::initialize_allocation_region;
    use crate::block::BLOCK_SIZE;
    use crate::directory_table::{initialize_directory_table_region, load_directory_table};
    use crate::format::{Superblock, SUPERBLOCK_BLOCK};
    use crate::fsck::check_device;
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
                return Err(io::Error::other(
                    "injected atomic-unlink home-write failure",
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

    fn desired_unlink(
        device: &mut FaultDevice,
        superblock: &Superblock,
        data_block: u64,
    ) -> (BlockAllocator, Vec<PersistedInode>) {
        let mut allocator = load_allocator(device, superblock).unwrap();
        allocator.free(data_block).unwrap();
        let remaining_inodes = vec![PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        }];
        (allocator, remaining_inodes)
    }

    #[test]
    fn atomic_unlink_removes_namespace_inode_and_block_ownership_together() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device);
        let (data_block, _, _) = seed_linked_file(&mut device, &superblock);
        let (allocator, remaining_inodes) = desired_unlink(&mut device, &superblock, data_block);

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
        let (allocator, remaining_inodes) = desired_unlink(&mut device, &superblock, data_block);
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

    #[test]
    fn rejects_unlink_that_leaves_target_referenced() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device);
        let (data_block, _, original_entries) = seed_linked_file(&mut device, &superblock);
        let (allocator, remaining_inodes) = desired_unlink(&mut device, &superblock, data_block);

        let error = store_unlink_metadata_journaled(
            &mut device,
            &superblock,
            &allocator,
            &remaining_inodes,
            &original_entries,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        check_device(&mut device).unwrap();
    }

    #[test]
    fn rejects_unlink_that_frees_unrelated_block() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device);
        let (data_block, _, _) = seed_linked_file(&mut device, &superblock);
        let mut allocator = load_allocator(&mut device, &superblock).unwrap();
        let extra = allocator.allocate().unwrap();
        let current_inodes = load_inode_table(&mut device, &superblock).unwrap();
        let current_entries = load_directory_table(&mut device, &superblock).unwrap();
        store_create_metadata_journaled(
            &mut device,
            &superblock,
            &allocator,
            &current_inodes,
            &current_entries,
        )
        .unwrap();
        let mut desired = load_allocator(&mut device, &superblock).unwrap();
        desired.free(data_block).unwrap();
        desired.free(extra).unwrap();
        let remaining_inodes = vec![PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        }];

        let error = store_unlink_metadata_journaled(
            &mut device,
            &superblock,
            &desired,
            &remaining_inodes,
            &[],
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(load_allocator(&mut device, &superblock)
            .unwrap()
            .is_owned(extra)
            .unwrap());
    }

    #[test]
    fn rejects_non_empty_directory_unlink() {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device);
        let allocator = load_allocator(&mut device, &superblock).unwrap();
        let inodes = vec![
            PersistedInode {
                id: 1,
                kind: InodeKind::Directory,
                blocks: Vec::new(),
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::Directory,
                blocks: Vec::new(),
            },
            PersistedInode {
                id: 3,
                kind: InodeKind::File,
                blocks: Vec::new(),
            },
        ];
        let entries = vec![
            PersistedDirectoryEntry {
                parent: 1,
                target: 2,
                name: "dir".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 2,
                target: 3,
                name: "child".to_owned(),
            },
        ];
        store_create_metadata_journaled(&mut device, &superblock, &allocator, &inodes, &entries)
            .unwrap();

        let desired_inodes = vec![inodes[0].clone(), inodes[2].clone()];
        let desired_entries = vec![entries[1].clone()];
        let error = store_unlink_metadata_journaled(
            &mut device,
            &superblock,
            &allocator,
            &desired_inodes,
            &desired_entries,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        check_device(&mut device).unwrap();
    }
}
