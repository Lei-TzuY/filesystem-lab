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
use filesystem_lab::path_hard_link::{
    hard_link_file_at_path_journaled, hard_link_symlink_at_path_journaled,
};
use filesystem_lab::path_lookup::{
    read_symlink_at_path, resolve_path_following_symlinks,
    resolve_path_without_following_final_symlink,
};
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
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

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "dir"), entry(1, 3, "file")],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/file").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn creates_regular_file_hard_link_through_source_and_parent_symlinks() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    hard_link_file_at_path_journaled(&mut device, &superblock, "/file_alias", "/dir_alias/linked")
        .unwrap();

    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/dir/linked").unwrap(),
        3
    );
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
fn creates_symlink_hard_link_without_following_final_source() {
    let (mut device, superblock) = setup();
    let source_inode =
        resolve_path_without_following_final_symlink(&mut device, &superblock, "/file_alias")
            .unwrap();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    hard_link_symlink_at_path_journaled(
        &mut device,
        &superblock,
        "/file_alias",
        "/dir_alias/symlink_link",
    )
    .unwrap();

    assert_eq!(
        resolve_path_without_following_final_symlink(
            &mut device,
            &superblock,
            "/dir/symlink_link",
        )
        .unwrap(),
        source_inode
    );
    assert_eq!(
        read_symlink_at_path(&mut device, &superblock, "/dir/symlink_link").unwrap(),
        "/file"
    );
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
fn symlink_hard_link_rejects_non_symlink_source_and_invalid_destinations() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();

    for (source, destination) in [
        ("file_alias", "/new"),
        ("/file", "/new"),
        ("/file_alias", "new"),
        ("/file_alias", "/"),
        ("/file_alias", "/dir/"),
        ("/file_alias", "/dir"),
    ] {
        assert_eq!(
            hard_link_symlink_at_path_journaled(&mut device, &superblock, source, destination)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        inodes_before
    );
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        entries_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn rejects_invalid_paths_and_destination_collisions_before_publication() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();

    for (source, destination) in [
        ("file", "/new"),
        ("/file", "new"),
        ("/file", "/"),
        ("/file", "/dir/"),
        ("/file", "/dir"),
    ] {
        assert_eq!(
            hard_link_file_at_path_journaled(&mut device, &superblock, source, destination)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        inodes_before
    );
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        entries_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_pathname_hard_link_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    hard_link_file_at_path_journaled(&mut probe, &superblock, "/file_alias", "/dir_alias/linked")
        .unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert_eq!(
            hard_link_file_at_path_journaled(
                &mut device,
                &superblock,
                "/file_alias",
                "/dir_alias/linked",
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt pathname hard-link creation"
        );
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
        let entries_after = load_directory_table(&mut device, &superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_eq!(entries_after, entries_before);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(entries_after.len(), entries_before.len() + 1);
            assert_eq!(
                resolve_path_following_symlinks(&mut device, &superblock, "/dir/linked").unwrap(),
                3
            );
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
fn every_pathname_symlink_hard_link_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    hard_link_symlink_at_path_journaled(
        &mut probe,
        &superblock,
        "/file_alias",
        "/dir_alias/symlink_link",
    )
    .unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let source_inode =
            resolve_path_without_following_final_symlink(&mut device, &superblock, "/file_alias")
                .unwrap();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();
        let target_before = read_symlink_at_path(&mut device, &superblock, "/file_alias").unwrap();

        device.arm(Some(crash_at));
        assert_eq!(
            hard_link_symlink_at_path_journaled(
                &mut device,
                &superblock,
                "/file_alias",
                "/dir_alias/symlink_link",
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt pathname symlink hard-link creation"
        );
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
        let entries_after = load_directory_table(&mut device, &superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_eq!(entries_after, entries_before);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(entries_after.len(), entries_before.len() + 1);
            assert_eq!(
                resolve_path_without_following_final_symlink(
                    &mut device,
                    &superblock,
                    "/dir/symlink_link",
                )
                .unwrap(),
                source_inode
            );
            assert_eq!(
                read_symlink_at_path(&mut device, &superblock, "/dir/symlink_link").unwrap(),
                target_before
            );
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
