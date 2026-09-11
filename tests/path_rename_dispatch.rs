mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_rename_dispatch::rename_posix_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 8;

fn inode(id: u64, kind: InodeKind) -> PersistedInode {
    PersistedInode {
        id,
        kind,
        blocks: Vec::new(),
    }
}

fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry {
        parent,
        target,
        name: name.to_owned(),
    }
}

fn setup_without_destination() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[inode(1, InodeKind::Directory), inode(2, InodeKind::File)],
    )
    .unwrap();
    store_directory_table(&mut device, &superblock, &[entry(1, 2, "source")]).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn setup_with_destination() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::File),
            inode(3, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "source"), entry(1, 3, "destination")],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn target_for(device: &mut CrashDevice, superblock: &Superblock, name: &str) -> Option<u64> {
    load_directory_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|entry| entry.parent == 1 && entry.name == name)
        .map(|entry| entry.target)
}

#[test]
fn moves_when_destination_is_absent() {
    let (mut device, superblock) = setup_without_destination();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    rename_posix_at_path_journaled(&mut device, &superblock, "/source", "/destination").unwrap();

    assert_eq!(target_for(&mut device, &superblock, "source"), None);
    assert_eq!(target_for(&mut device, &superblock, "destination"), Some(2));
    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        inodes_before
    );
    check_device(&mut device).unwrap();
}

#[test]
fn overwrites_existing_same_kind_destination() {
    let (mut device, superblock) = setup_with_destination();

    rename_posix_at_path_journaled(&mut device, &superblock, "/source", "/destination").unwrap();

    assert_eq!(target_for(&mut device, &superblock, "source"), None);
    assert_eq!(target_for(&mut device, &superblock, "destination"), Some(2));
    assert!(load_inode_table(&mut device, &superblock)
        .unwrap()
        .iter()
        .all(|inode| inode.id != 3));
    check_device(&mut device).unwrap();
}

#[test]
fn same_inode_alias_is_a_no_op() {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[inode(1, InodeKind::Directory), inode(2, InodeKind::File)],
    )
    .unwrap();
    let before = vec![entry(1, 2, "source"), entry(1, 2, "destination")];
    store_directory_table(&mut device, &superblock, &before).unwrap();

    assert_eq!(
        rename_posix_at_path_journaled(&mut device, &superblock, "/source", "/destination")
            .unwrap(),
        RecoveryReport::default()
    );

    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn alias_no_op_still_enforces_directory_intent() {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[inode(1, InodeKind::Directory), inode(2, InodeKind::File)],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "source"), entry(1, 2, "destination")],
    )
    .unwrap();

    assert_eq!(
        rename_posix_at_path_journaled(&mut device, &superblock, "/source/", "/destination")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn every_absent_destination_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup_without_destination();
    probe.arm(None);
    rename_posix_at_path_journaled(&mut probe, &superblock, "/source", "/destination").unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup_without_destination();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert!(rename_posix_at_path_journaled(
            &mut device,
            &superblock,
            "/source",
            "/destination",
        )
        .is_err());
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        assert_eq!(
            load_allocator(&mut device, &superblock).unwrap(),
            allocator_before
        );
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap(),
            inodes_before
        );
        if recovery.committed_transactions == 0 {
            assert_eq!(
                load_directory_table(&mut device, &superblock).unwrap(),
                entries_before
            );
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(target_for(&mut device, &superblock, "source"), None);
            assert_eq!(target_for(&mut device, &superblock, "destination"), Some(2));
        }
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }
}

#[test]
fn every_overwrite_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup_with_destination();
    probe.arm(None);
    rename_posix_at_path_journaled(&mut probe, &superblock, "/source", "/destination").unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup_with_destination();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert!(rename_posix_at_path_journaled(
            &mut device,
            &superblock,
            "/source",
            "/destination",
        )
        .is_err());
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        assert_eq!(
            load_allocator(&mut device, &superblock).unwrap(),
            allocator_before
        );
        if recovery.committed_transactions == 0 {
            assert_eq!(
                load_inode_table(&mut device, &superblock).unwrap(),
                inodes_before
            );
            assert_eq!(
                load_directory_table(&mut device, &superblock).unwrap(),
                entries_before
            );
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            let inodes = load_inode_table(&mut device, &superblock).unwrap();
            assert!(inodes.iter().any(|inode| inode.id == 2));
            assert!(inodes.iter().all(|inode| inode.id != 3));
            assert_eq!(target_for(&mut device, &superblock, "source"), None);
            assert_eq!(target_for(&mut device, &superblock, "destination"), Some(2));
        }
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }
}
