mod support;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal::JournalEntry;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_lookup::resolve_path_following_symlinks;
use filesystem_lab::path_rename::rename_exchange_files_at_path_journaled;
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

fn setup_without_right_alias() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::Directory),
            inode(4, InodeKind::File),
            inode(5, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 2, "left"),
            entry(1, 3, "right"),
            entry(2, 4, "a"),
            entry(3, 5, "b"),
        ],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "left_alias", "/left").unwrap();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn setup() -> (CrashDevice, Superblock) {
    let (mut device, superblock) = setup_without_right_alias();
    create_symlink_journaled(&mut device, &superblock, 1, "right_alias", "/right").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn has_commit(entries: &[JournalEntry]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry, JournalEntry::Commit { .. }))
}

#[test]
fn exchanges_regular_files_through_symlinked_parents() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    rename_exchange_files_at_path_journaled(
        &mut device,
        &superblock,
        "/left_alias/a",
        "/right_alias/b",
    )
    .unwrap();

    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/left/a").unwrap(),
        5
    );
    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/right/b").unwrap(),
        4
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
fn exchange_recovers_committed_parent_symlink_before_resolving_it() {
    let (mut probe, superblock) = setup_without_right_alias();
    probe.arm(None);
    create_symlink_journaled(&mut probe, &superblock, 1, "right_alias", "/right").unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock) = setup_without_right_alias();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        if create_symlink_journaled(&mut device, &superblock, 1, "right_alias", "/right").is_ok() {
            continue;
        }
        device.reboot();
        let durable_journal = load_journal_image(&mut device, superblock).unwrap();
        if !has_commit(&durable_journal) {
            continue;
        }
        committed_crash_states += 1;

        rename_exchange_files_at_path_journaled(
            &mut device,
            &superblock,
            "/left_alias/a",
            "/right_alias/b",
        )
        .unwrap();

        assert_eq!(
            resolve_path_following_symlinks(&mut device, &superblock, "/left/a").unwrap(),
            5
        );
        assert_eq!(
            resolve_path_following_symlinks(&mut device, &superblock, "/right/b").unwrap(),
            4
        );
        assert_eq!(
            load_allocator(&mut device, &superblock).unwrap(),
            allocator_before + 1
        );
        let inodes_after = load_inode_table(&mut device, &superblock).unwrap();
        assert_eq!(inodes_after.len(), inodes_before.len() + 1);
        check_device(&mut device).unwrap();

        recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }

    assert!(committed_crash_states > 0);
}

#[test]
fn every_pathname_exchange_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    rename_exchange_files_at_path_journaled(
        &mut probe,
        &superblock,
        "/left_alias/a",
        "/right_alias/b",
    )
    .unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert!(rename_exchange_files_at_path_journaled(
            &mut device,
            &superblock,
            "/left_alias/a",
            "/right_alias/b",
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
            assert_eq!(
                resolve_path_following_symlinks(&mut device, &superblock, "/left/a").unwrap(),
                5
            );
            assert_eq!(
                resolve_path_following_symlinks(&mut device, &superblock, "/right/b").unwrap(),
                4
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
