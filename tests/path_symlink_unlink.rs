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
use filesystem_lab::path_lookup::read_symlink_at_path;
use filesystem_lab::path_symlink::{
    create_symlink_at_path_journaled, unlink_symlink_at_path_journaled,
};
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 8;
const TARGET: &str = "../target/file";

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
        ],
    )
    .unwrap();
    store_directory_table(&mut device, &superblock, &[entry(1, 2, "dir")]).unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    create_symlink_at_path_journaled(&mut device, &superblock, "/dir/link", TARGET).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn unlinks_final_symlink_without_following_it() {
    let (mut device, superblock) = setup();
    let allocated_before = load_allocator(&mut device, &superblock)
        .unwrap()
        .allocated_blocks();

    unlink_symlink_at_path_journaled(&mut device, &superblock, "/dir_alias/link").unwrap();

    assert_eq!(
        read_symlink_at_path(&mut device, &superblock, "/dir/link")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        allocated_before - 1
    );
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_invalid_path_forms_without_publishing() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();

    for path in ["relative", "/", "/dir/"] {
        assert_eq!(
            unlink_symlink_at_path_journaled(&mut device, &superblock, path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        unlink_symlink_at_path_journaled(&mut device, &superblock, "/missing/link")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
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
fn every_pathname_symlink_unlink_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    unlink_symlink_at_path_journaled(&mut probe, &superblock, "/dir_alias/link").unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert_eq!(
            unlink_symlink_at_path_journaled(&mut device, &superblock, "/dir_alias/link")
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
            assert_eq!(
                read_symlink_at_path(&mut device, &superblock, "/dir/link").unwrap(),
                TARGET
            );
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(
                read_symlink_at_path(&mut device, &superblock, "/dir/link")
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::NotFound
            );
            assert_eq!(
                load_allocator(&mut device, &superblock)
                    .unwrap()
                    .allocated_blocks(),
                allocator_before.allocated_blocks() - 1
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
