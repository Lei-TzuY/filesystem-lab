mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::create_tx::store_create_metadata_journaled;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::load_directory_table;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use support::CrashDevice;

fn root_inode() -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }
}

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(&mut device, &superblock, &[root_inode()]).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn desired_create(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> (
    filesystem_lab::allocation::BlockAllocator,
    u64,
    Vec<PersistedInode>,
    Vec<PersistedDirectoryEntry>,
) {
    let mut allocator = load_allocator(device, superblock).unwrap();
    let data_block = allocator.allocate().unwrap();
    let inodes = vec![
        root_inode(),
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

fn assert_old_state(device: &mut CrashDevice, superblock: &Superblock, data_block: u64) {
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

fn assert_new_state(
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

#[test]
fn every_create_mutation_crash_point_is_old_or_recoverable_new_state() {
    let (mut probe, superblock) = setup();
    let (allocator, data_block, inodes, entries) = desired_create(&mut probe, &superblock);
    probe.arm(None);
    store_create_metadata_journaled(&mut probe, &superblock, &allocator, &inodes, &entries)
        .unwrap();
    let mutation_operations = probe.operations();

    assert!(
        mutation_operations >= 7,
        "atomic create must cross journal, home-write, and checkpoint durability operations"
    );
    assert_new_state(&mut probe, &superblock, data_block, &inodes, &entries);
    assert!(load_journal_image(&mut probe, superblock).unwrap().is_empty());
    check_device(&mut probe).unwrap();

    for crash_at in 0..mutation_operations {
        let (mut device, superblock) = setup();
        let (allocator, data_block, inodes, entries) = desired_create(&mut device, &superblock);
        device.arm(Some(crash_at));

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
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt the create"
        );

        device.reboot();

        let raw_is_old = {
            let allocator = load_allocator(&mut device, &superblock).unwrap();
            let block_is_owned = allocator.is_owned(data_block).unwrap();
            let disk_inodes = load_inode_table(&mut device, &superblock).unwrap();
            let disk_entries = load_directory_table(&mut device, &superblock).unwrap();
            !block_is_owned && disk_inodes == vec![root_inode()] && disk_entries.is_empty()
        };
        let raw_is_new = {
            let allocator = load_allocator(&mut device, &superblock).unwrap();
            allocator.is_owned(data_block).unwrap()
                && load_inode_table(&mut device, &superblock).unwrap() == inodes
                && load_directory_table(&mut device, &superblock).unwrap() == entries
        };

        if raw_is_old || raw_is_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(
                check_device(&mut device).is_err(),
                "crash point {crash_at} exposed a partial create state that fsck accepted"
            );
        }

        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if report.committed_transactions == 0 {
            assert_old_state(&mut device, &superblock, data_block);
        } else {
            assert_eq!(report.committed_transactions, 1);
            assert_eq!(report.home_writes, 3);
            assert_new_state(&mut device, &superblock, data_block, &inodes, &entries);
        }
        assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
        check_device(&mut device).unwrap();

        let second_replay = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second_replay, Default::default());
        assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
        if report.committed_transactions == 0 {
            assert_old_state(&mut device, &superblock, data_block);
        } else {
            assert_new_state(&mut device, &superblock, data_block, &inodes, &entries);
        }
    }
}

#[test]
fn successful_create_checkpoints_before_fixed_journal_reuse() {
    let (mut device, superblock) = setup();
    let (allocator, first_block, inodes, entries) = desired_create(&mut device, &superblock);

    let first = store_create_metadata_journaled(
        &mut device,
        &superblock,
        &allocator,
        &inodes,
        &entries,
    )
    .unwrap();
    assert_eq!(first.committed_transactions, 1);
    assert_eq!(first.home_writes, 3);
    assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());

    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let second_block = allocator.allocate().unwrap();
    let mut second_inodes = inodes;
    second_inodes.push(PersistedInode {
        id: 3,
        kind: InodeKind::File,
        blocks: vec![second_block],
    });
    let mut second_entries = entries;
    second_entries.push(PersistedDirectoryEntry {
        parent: 1,
        target: 3,
        name: "second".to_owned(),
    });

    let second = store_create_metadata_journaled(
        &mut device,
        &superblock,
        &allocator,
        &second_inodes,
        &second_entries,
    )
    .unwrap();
    assert_eq!(second.committed_transactions, 1);
    assert_eq!(second.home_writes, 3);
    assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
    assert!(load_allocator(&mut device, &superblock)
        .unwrap()
        .is_owned(first_block)
        .unwrap());
    assert!(load_allocator(&mut device, &superblock)
        .unwrap()
        .is_owned(second_block)
        .unwrap());
    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), second_inodes);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        second_entries
    );
    check_device(&mut device).unwrap();
}
