mod support;

use std::collections::HashSet;
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
use filesystem_lab::path_unlink::remove_directory_at_path_journaled;
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
            inode(3, InodeKind::Directory),
            inode(4, InodeKind::Directory),
            inode(5, InodeKind::File),
            inode(6, InodeKind::Directory),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 2, "parent"),
            entry(2, 3, "empty"),
            entry(2, 4, "nonempty"),
            entry(4, 5, "child"),
            entry(2, 6, "empty2"),
        ],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "alias", "/parent").unwrap();
    create_symlink_journaled(
        &mut device,
        &superblock,
        1,
        "final_link",
        "/parent/empty",
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn assert_unique_ownership(device: &mut CrashDevice, superblock: &Superblock) {
    let allocator = load_allocator(device, superblock).unwrap();
    allocator.validate().unwrap();
    let inodes = load_inode_table(device, superblock).unwrap();
    let mut seen = HashSet::new();
    for inode in &inodes {
        for block in &inode.blocks {
            assert!(
                seen.insert(*block),
                "duplicate physical block reference {block}"
            );
            assert!(allocator.is_owned(*block).unwrap());
        }
    }
}

#[test]
fn removes_empty_directories_through_direct_and_symlinked_parents() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();

    remove_directory_at_path_journaled(&mut device, &superblock, "/parent/empty").unwrap();
    remove_directory_at_path_journaled(&mut device, &superblock, "/alias/empty2").unwrap();

    let inodes = load_inode_table(&mut device, &superblock).unwrap();
    assert!(!inodes.iter().any(|inode| inode.id == 3 || inode.id == 6));
    let entries = load_directory_table(&mut device, &superblock).unwrap();
    assert!(!entries
        .iter()
        .any(|entry| entry.target == 3 || entry.target == 6));
    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_unique_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_root_nonempty_non_directory_final_symlink_and_malformed_paths() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();

    for path in ["relative", "/", "/parent/"] {
        assert_eq!(
            remove_directory_at_path_journaled(&mut device, &superblock, path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    for path in ["/parent/nonempty", "/parent/nonempty/child", "/final_link"] {
        assert_eq!(
            remove_directory_at_path_journaled(&mut device, &superblock, path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes_before);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        entries_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_pathname_directory_remove_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    remove_directory_at_path_journaled(&mut probe, &superblock, "/alias/empty").unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert_eq!(
            remove_directory_at_path_journaled(&mut device, &superblock, "/alias/empty")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt pathname directory removal"
        );
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        let inodes_after = load_inode_table(&mut device, &superblock).unwrap();
        let entries_after = load_directory_table(&mut device, &superblock).unwrap();
        assert_eq!(allocator_after, allocator_before);

        if recovery.committed_transactions == 0 {
            assert_eq!(inodes_after, inodes_before);
            assert_eq!(entries_after, entries_before);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(inodes_after.len(), inodes_before.len() - 1);
            assert_eq!(entries_after.len(), entries_before.len() - 1);
            assert!(!inodes_after.iter().any(|inode| inode.id == 3));
            assert!(!entries_after.iter().any(|entry| entry.target == 3));
        }

        assert_unique_ownership(&mut device, &superblock);
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
