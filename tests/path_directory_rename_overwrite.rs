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
use filesystem_lab::path_rename_overwrite::rename_overwrite_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 12;

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
    let mut device = CrashDevice::new(160);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::Directory),
            inode(4, InodeKind::Directory),
            inode(5, InodeKind::Directory),
            inode(6, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 2, "src_parent"),
            entry(1, 3, "dst_parent"),
            entry(2, 4, "source"),
            entry(3, 5, "target"),
            entry(4, 6, "child"),
        ],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn assert_new_state(device: &mut CrashDevice, superblock: &Superblock) {
    assert!(
        resolve_path_following_symlinks(device, superblock, "/src_parent/source").is_err()
    );
    assert_eq!(
        resolve_path_following_symlinks(device, superblock, "/dst_parent/target").unwrap(),
        4
    );
    assert_eq!(
        resolve_path_following_symlinks(device, superblock, "/dst_parent/target/child").unwrap(),
        6
    );
    assert!(!load_inode_table(device, superblock)
        .unwrap()
        .iter()
        .any(|inode| inode.id == 5));
}

#[test]
fn dispatch_overwrites_empty_directory_with_terminal_slashes() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();

    rename_overwrite_at_path_journaled(
        &mut device,
        &superblock,
        "/src_parent/source/",
        "/dst_parent/target/",
    )
    .unwrap();

    assert_new_state(&mut device, &superblock);
    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn dispatch_rejects_repeated_trailing_separators_without_publication() {
    let (mut device, superblock) = setup();
    let before = load_directory_table(&mut device, &superblock).unwrap();

    assert_eq!(
        rename_overwrite_at_path_journaled(
            &mut device,
            &superblock,
            "/src_parent/source//",
            "/dst_parent/target/",
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn dispatch_rejects_nonempty_destination_without_publication() {
    let (mut device, superblock) = setup();
    let mut entries = load_directory_table(&mut device, &superblock).unwrap();
    entries.push(entry(5, 6, "occupant"));
    store_directory_table(&mut device, &superblock, &entries).unwrap();
    let before = load_directory_table(&mut device, &superblock).unwrap();
    assert_eq!(
        rename_overwrite_at_path_journaled(
            &mut device,
            &superblock,
            "/src_parent/source/",
            "/dst_parent/target/",
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_dispatch_directory_overwrite_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    rename_overwrite_at_path_journaled(
        &mut probe,
        &superblock,
        "/src_parent/source/",
        "/dst_parent/target/",
    )
    .unwrap();
    let operations = probe.operations();
    assert!(operations >= 6);

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();
        device.arm(Some(crash_at));
        assert_eq!(
            rename_overwrite_at_path_journaled(
                &mut device,
                &superblock,
                "/src_parent/source/",
                "/dst_parent/target/",
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
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
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_new_state(&mut device, &superblock);
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
