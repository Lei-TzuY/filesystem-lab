mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::hard_link_tx::hard_link_file_journaled;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_lookup::resolve_path_following_symlinks;
use filesystem_lab::path_unlink::unlink_file_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 8;

fn inode(id: u64, kind: InodeKind, blocks: Vec<u64>) -> PersistedInode {
    PersistedInode { id, kind, blocks }
}

fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry {
        parent,
        target,
        name: name.to_owned(),
    }
}

fn setup() -> (CrashDevice, Superblock, [u64; 2]) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let first = allocator.allocate().unwrap();
    let second = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory, Vec::new()),
            inode(2, InodeKind::Directory, Vec::new()),
            inode(3, InodeKind::File, vec![first, second]),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "dir"), entry(2, 3, "file")],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, [first, second])
}

fn assert_file_present(device: &mut CrashDevice, superblock: &Superblock, blocks: [u64; 2]) {
    assert_eq!(
        resolve_path_following_symlinks(device, superblock, "/dir/file").unwrap(),
        3
    );
    let inodes = load_inode_table(device, superblock).unwrap();
    let file = inodes.iter().find(|inode| inode.id == 3).unwrap();
    assert_eq!(file.kind, InodeKind::File);
    assert_eq!(file.blocks, blocks);
    let allocator = load_allocator(device, superblock).unwrap();
    for block in blocks {
        assert!(allocator.is_owned(block).unwrap());
    }
}

fn assert_file_absent(device: &mut CrashDevice, superblock: &Superblock, blocks: [u64; 2]) {
    assert_eq!(
        resolve_path_following_symlinks(device, superblock, "/dir/file")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert!(load_inode_table(device, superblock)
        .unwrap()
        .iter()
        .all(|inode| inode.id != 3));
    let allocator = load_allocator(device, superblock).unwrap();
    for block in blocks {
        assert!(!allocator.is_owned(block).unwrap());
    }
}

#[test]
fn unlinks_regular_file_through_symlinked_parent_and_frees_all_owned_blocks() {
    let (mut device, superblock, blocks) = setup();
    let allocated_before = load_allocator(&mut device, &superblock)
        .unwrap()
        .allocated_blocks();

    unlink_file_at_path_journaled(&mut device, &superblock, "/dir_alias/file").unwrap();

    assert_file_absent(&mut device, &superblock, blocks);
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        allocated_before - 2
    );
    assert!(load_directory_table(&mut device, &superblock)
        .unwrap()
        .iter()
        .all(|entry| !(entry.parent == 2 && entry.name == "file")));
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_final_symlink_and_multiply_linked_file_without_publishing() {
    let (mut device, superblock, blocks) = setup();
    create_symlink_journaled(&mut device, &superblock, 2, "link", "/dir/file").unwrap();
    assert_eq!(
        unlink_file_at_path_journaled(&mut device, &superblock, "/dir/link")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_file_present(&mut device, &superblock, blocks);

    hard_link_file_journaled(&mut device, &superblock, 2, "alias", 3).unwrap();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();
    assert_eq!(
        unlink_file_at_path_journaled(&mut device, &superblock, "/dir/file")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
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
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_invalid_path_forms_without_publishing() {
    let (mut device, superblock, blocks) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();

    for path in ["relative", "/", "/dir/"] {
        assert_eq!(
            unlink_file_at_path_journaled(&mut device, &superblock, path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        unlink_file_at_path_journaled(&mut device, &superblock, "/missing/file")
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
    assert_file_present(&mut device, &superblock, blocks);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_pathname_file_unlink_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock, _) = setup();
    probe.arm(None);
    unlink_file_at_path_journaled(&mut probe, &superblock, "/dir_alias/file").unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock, blocks) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert_eq!(
            unlink_file_at_path_journaled(&mut device, &superblock, "/dir_alias/file")
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
            assert_file_present(&mut device, &superblock, blocks);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_file_absent(&mut device, &superblock, blocks);
            assert_eq!(
                load_allocator(&mut device, &superblock)
                    .unwrap()
                    .allocated_blocks(),
                allocator_before.allocated_blocks() - 2
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
