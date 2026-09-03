mod support;

use std::io;

use filesystem_lab::allocation::BlockAllocator;
use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::recovery::recover_journal;
use filesystem_lab::unlink_tx::store_unlink_metadata_journaled;
use support::CrashDevice;

fn root_inode() -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }
}

fn setup() -> (
    CrashDevice,
    Superblock,
    u64,
    Vec<PersistedInode>,
    Vec<PersistedDirectoryEntry>,
) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();

    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let data_block = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();

    let inodes = vec![
        root_inode(),
        PersistedInode {
            id: 2,
            kind: InodeKind::File,
            blocks: vec![data_block],
        },
    ];
    store_inode_table(&mut device, &superblock, &inodes).unwrap();

    let entries = vec![PersistedDirectoryEntry {
        parent: 1,
        target: 2,
        name: "child".to_owned(),
    }];
    store_directory_table(&mut device, &superblock, &entries).unwrap();
    check_device(&mut device).unwrap();

    (device, superblock, data_block, inodes, entries)
}

fn desired_unlink(
    device: &mut CrashDevice,
    superblock: &Superblock,
    data_block: u64,
) -> (
    BlockAllocator,
    Vec<PersistedInode>,
    Vec<PersistedDirectoryEntry>,
) {
    let mut allocator = load_allocator(device, superblock).unwrap();
    allocator.free(data_block).unwrap();
    (allocator, vec![root_inode()], Vec::new())
}

fn assert_old_state(
    device: &mut CrashDevice,
    superblock: &Superblock,
    data_block: u64,
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
) {
    assert!(load_allocator(device, superblock)
        .unwrap()
        .is_owned(data_block)
        .unwrap());
    assert_eq!(load_inode_table(device, superblock).unwrap(), inodes);
    assert_eq!(load_directory_table(device, superblock).unwrap(), entries);
}

fn assert_new_state(device: &mut CrashDevice, superblock: &Superblock, data_block: u64) {
    assert!(!load_allocator(device, superblock)
        .unwrap()
        .is_owned(data_block)
        .unwrap());
    assert_eq!(
        load_inode_table(device, superblock).unwrap(),
        vec![root_inode()]
    );
    assert!(load_directory_table(device, superblock).unwrap().is_empty());
}

#[test]
fn every_unlink_mutation_crash_point_is_old_or_recoverable_new_state() {
    let (mut probe, superblock, data_block, _, _) = setup();
    let (allocator, remaining_inodes, remaining_entries) =
        desired_unlink(&mut probe, &superblock, data_block);
    probe.arm(None);
    store_unlink_metadata_journaled(
        &mut probe,
        &superblock,
        &allocator,
        &remaining_inodes,
        &remaining_entries,
    )
    .unwrap();
    let mutation_operations = probe.operations();

    assert!(
        mutation_operations >= 5,
        "atomic unlink must cross journal and three home-write durability operations"
    );
    assert_new_state(&mut probe, &superblock, data_block);
    check_device(&mut probe).unwrap();

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, data_block, original_inodes, original_entries) = setup();
        let (allocator, remaining_inodes, remaining_entries) =
            desired_unlink(&mut device, &superblock, data_block);
        device.arm(Some(crash_at));

        assert_eq!(
            store_unlink_metadata_journaled(
                &mut device,
                &superblock,
                &allocator,
                &remaining_inodes,
                &remaining_entries,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt the unlink"
        );

        device.reboot();

        let raw_is_old = {
            let allocator = load_allocator(&mut device, &superblock).unwrap();
            allocator.is_owned(data_block).unwrap()
                && load_inode_table(&mut device, &superblock).unwrap() == original_inodes
                && load_directory_table(&mut device, &superblock).unwrap() == original_entries
        };
        let raw_is_new = {
            let allocator = load_allocator(&mut device, &superblock).unwrap();
            !allocator.is_owned(data_block).unwrap()
                && load_inode_table(&mut device, &superblock).unwrap() == vec![root_inode()]
                && load_directory_table(&mut device, &superblock)
                    .unwrap()
                    .is_empty()
        };

        if raw_is_old || raw_is_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(
                check_device(&mut device).is_err(),
                "crash point {crash_at} exposed a partial unlink state that fsck accepted"
            );
        }

        let report = recover_journal(&mut device, superblock).unwrap();
        if report.committed_transactions == 0 {
            assert_old_state(
                &mut device,
                &superblock,
                data_block,
                &original_inodes,
                &original_entries,
            );
        } else {
            assert_eq!(report.committed_transactions, 1);
            assert_eq!(report.home_writes, 3);
            assert_new_state(&mut device, &superblock, data_block);
        }
        check_device(&mut device).unwrap();

        let second_replay = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(second_replay, report);
        if report.committed_transactions == 0 {
            assert_old_state(
                &mut device,
                &superblock,
                data_block,
                &original_inodes,
                &original_entries,
            );
        } else {
            assert_new_state(&mut device, &superblock, data_block);
        }
    }
}
