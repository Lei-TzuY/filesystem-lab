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
use filesystem_lab::path_lookup::resolve_path_following_symlinks;
use filesystem_lab::path_rename_exchange_dispatch::rename_exchange_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::{create_symlink_journaled, read_symlink};
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

fn setup_files() -> (CrashDevice, Superblock) {
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
        &[entry(1, 2, "left"), entry(1, 3, "right")],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn target_for(device: &mut CrashDevice, superblock: &Superblock, name: &str) -> u64 {
    load_directory_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|entry| entry.parent == 1 && entry.name == name)
        .unwrap()
        .target
}

#[test]
fn dispatches_regular_file_exchange() {
    let (mut device, superblock) = setup_files();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    rename_exchange_at_path_journaled(&mut device, &superblock, "/left", "/right").unwrap();

    assert_eq!(target_for(&mut device, &superblock, "left"), 3);
    assert_eq!(target_for(&mut device, &superblock, "right"), 2);
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
fn dispatches_final_symlink_exchange_without_following_targets() {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[inode(1, InodeKind::Directory)],
    )
    .unwrap();
    let (left, _) =
        create_symlink_journaled(&mut device, &superblock, 1, "left", "/missing/a").unwrap();
    let (right, _) =
        create_symlink_journaled(&mut device, &superblock, 1, "right", "/missing/b").unwrap();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();

    rename_exchange_at_path_journaled(&mut device, &superblock, "/left", "/right").unwrap();

    assert_eq!(target_for(&mut device, &superblock, "left"), right);
    assert_eq!(target_for(&mut device, &superblock, "right"), left);
    assert_eq!(
        read_symlink(&mut device, &superblock, left).unwrap(),
        "/missing/a"
    );
    assert_eq!(
        read_symlink(&mut device, &superblock, right).unwrap(),
        "/missing/b"
    );
    check_device(&mut device).unwrap();
}

#[test]
fn dispatches_directory_exchange_with_terminal_slash_intent() {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::Directory),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "left"), entry(1, 3, "right")],
    )
    .unwrap();

    rename_exchange_at_path_journaled(&mut device, &superblock, "/left/", "/right/").unwrap();

    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/left").unwrap(),
        3
    );
    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/right").unwrap(),
        2
    );
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_mixed_kinds_and_repeated_trailing_separator_before_wal() {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::File),
            inode(3, InodeKind::Directory),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "file"), entry(1, 3, "dir")],
    )
    .unwrap();

    assert_eq!(
        rename_exchange_at_path_journaled(&mut device, &superblock, "/file", "/dir")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        rename_exchange_at_path_journaled(&mut device, &superblock, "/dir//", "/dir")
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
fn every_dispatched_file_exchange_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup_files();
    probe.arm(None);
    rename_exchange_at_path_journaled(&mut probe, &superblock, "/left", "/right").unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup_files();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert!(rename_exchange_at_path_journaled(
            &mut device,
            &superblock,
            "/left",
            "/right",
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
            assert_eq!(target_for(&mut device, &superblock, "left"), 3);
            assert_eq!(target_for(&mut device, &superblock, "right"), 2);
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
