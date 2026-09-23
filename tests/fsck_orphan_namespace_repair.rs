mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::fsck_repair::repair_unreachable_inodes_journaled;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::journal_region::load_journal_image;
use support::CrashDevice;

fn setup_orphan_subtree(with_collision: bool) -> (CrashDevice, Superblock, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let file_block = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    device.write_block(file_block, &[0x5a; BLOCK_SIZE]).unwrap();
    device.flush().unwrap();

    let mut inodes = vec![
        PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap(),
        PersistedInode::new(2, InodeKind::Directory, Vec::new()).unwrap(),
        PersistedInode::new(3, InodeKind::File, vec![file_block]).unwrap(),
    ];
    let mut entries = vec![PersistedDirectoryEntry {
        parent: 2,
        target: 3,
        name: "child".to_owned(),
    }];

    if with_collision {
        inodes.push(PersistedInode::new(4, InodeKind::File, Vec::new()).unwrap());
        entries.push(PersistedDirectoryEntry {
            parent: 1,
            target: 4,
            name: ".fsck-orphan-2".to_owned(),
        });
    }

    store_inode_table(&mut device, &superblock, &inodes).unwrap();
    store_directory_table(&mut device, &superblock, &entries).unwrap();

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("unreachable"));
    (device, superblock, file_block)
}

fn assert_repaired(
    device: &mut CrashDevice,
    superblock: &Superblock,
    file_block: u64,
    expected_name: &str,
) {
    let entries = load_directory_table(device, superblock).unwrap();
    assert!(entries.iter().any(|entry| {
        entry.parent == 1 && entry.target == 2 && entry.name == expected_name
    }));
    assert!(entries
        .iter()
        .any(|entry| entry.parent == 2 && entry.target == 3 && entry.name == "child"));

    let allocator = load_allocator(device, superblock).unwrap();
    assert!(allocator.is_owned(file_block).unwrap());

    let mut image = [0_u8; BLOCK_SIZE];
    device.read_block(file_block, &mut image).unwrap();
    assert_eq!(image, [0x5a; BLOCK_SIZE]);

    check_device(device).unwrap();
    assert!(load_journal_image(device, *superblock).unwrap().is_empty());
}

#[test]
fn repair_reattaches_only_orphan_component_root_and_is_idempotent() {
    let (mut device, superblock, file_block) = setup_orphan_subtree(false);

    let report = repair_unreachable_inodes_journaled(&mut device, &superblock).unwrap();

    assert_eq!(report.reattached.len(), 1);
    assert_eq!(report.reattached[0].inode_id, 2);
    assert_eq!(report.reattached[0].name, ".fsck-orphan-2");
    assert_eq!(report.repair_transaction.committed_transactions, 1);
    assert_repaired(&mut device, &superblock, file_block, ".fsck-orphan-2");

    device.arm(None);
    let second = repair_unreachable_inodes_journaled(&mut device, &superblock).unwrap();
    assert!(second.reattached.is_empty());
    assert_eq!(second.repair_transaction.committed_transactions, 0);
    assert_eq!(device.operations(), 0);
}

#[test]
fn repair_uses_deterministic_collision_suffix() {
    let (mut device, superblock, file_block) = setup_orphan_subtree(true);

    let report = repair_unreachable_inodes_journaled(&mut device, &superblock).unwrap();

    assert_eq!(report.reattached.len(), 1);
    assert_eq!(report.reattached[0].inode_id, 2);
    assert_eq!(report.reattached[0].name, ".fsck-orphan-2-1");
    assert_repaired(
        &mut device,
        &superblock,
        file_block,
        ".fsck-orphan-2-1",
    );
}

#[test]
fn orphan_directory_cycle_is_rejected_without_mutation() {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap(),
            PersistedInode::new(2, InodeKind::Directory, Vec::new()).unwrap(),
            PersistedInode::new(3, InodeKind::Directory, Vec::new()).unwrap(),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            PersistedDirectoryEntry {
                parent: 2,
                target: 3,
                name: "to-three".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 3,
                target: 2,
                name: "to-two".to_owned(),
            },
        ],
    )
    .unwrap();

    device.arm(None);
    let error = repair_unreachable_inodes_journaled(&mut device, &superblock).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("directory cycle"));
    assert_eq!(device.operations(), 0);
}

#[test]
fn every_repair_crash_point_converges_to_same_clean_namespace() {
    let (prepared, superblock, file_block) = setup_orphan_subtree(false);

    let mut probe = prepared.clone();
    probe.arm(None);
    repair_unreachable_inodes_journaled(&mut probe, &superblock).unwrap();
    let mutation_count = probe.operations();
    assert!(mutation_count > 0);

    for crash_at in 0..mutation_count {
        let mut device = prepared.clone();
        device.arm(Some(crash_at));
        assert!(
            repair_unreachable_inodes_journaled(&mut device, &superblock).is_err(),
            "crash_at={crash_at}"
        );

        device.reboot();
        repair_unreachable_inodes_journaled(&mut device, &superblock).unwrap();
        assert_repaired(
            &mut device,
            &superblock,
            file_block,
            ".fsck-orphan-2",
        );

        let second = repair_unreachable_inodes_journaled(&mut device, &superblock).unwrap();
        assert!(second.reattached.is_empty(), "crash_at={crash_at}");
    }
}
