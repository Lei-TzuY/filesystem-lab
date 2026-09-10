mod support;

use std::collections::HashSet;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal::JournalEntry;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_lookup::{read_symlink_at_path, resolve_path_without_following_final_symlink};
use filesystem_lab::path_symlink::{create_symlink_at_path_journaled, unlink_symlink_at_path_journaled};
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 10;

fn inode(id: u64, kind: InodeKind) -> PersistedInode {
    PersistedInode { id, kind, blocks: Vec::new() }
}

fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry { parent, target, name: name.to_owned() }
}

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[inode(1, InodeKind::Directory), inode(2, InodeKind::Directory)],
    )
    .unwrap();
    store_directory_table(&mut device, &superblock, &[entry(1, 2, "dir")]).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn setup_with_victim() -> (CrashDevice, Superblock) {
    let (mut device, superblock) = setup();
    create_symlink_journaled(&mut device, &superblock, 2, "victim", "/target").unwrap();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn has_commit(entries: &[JournalEntry]) -> bool {
    entries.iter().any(|entry| matches!(entry, JournalEntry::Commit { .. }))
}

fn assert_unique_ownership(device: &mut CrashDevice, superblock: &Superblock) {
    let allocator = load_allocator(device, superblock).unwrap();
    allocator.validate().unwrap();
    let inodes = load_inode_table(device, superblock).unwrap();
    let mut seen = HashSet::new();
    for inode in &inodes {
        for block in &inode.blocks {
            assert!(seen.insert(*block), "duplicate physical block reference {block}");
            assert!(allocator.is_owned(*block).unwrap());
        }
    }
}

#[test]
fn create_symlink_recovers_committed_parent_symlink_before_resolution() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    create_symlink_journaled(&mut probe, &superblock, 1, "dir_alias", "/dir").unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocated_before = load_allocator(&mut device, &superblock).unwrap().allocated_blocks();
        let inode_count_before = load_inode_table(&mut device, &superblock).unwrap().len();

        device.arm(Some(crash_at));
        if create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").is_ok() {
            continue;
        }
        device.reboot();
        if !has_commit(&load_journal_image(&mut device, superblock).unwrap()) {
            continue;
        }
        committed_crash_states += 1;

        create_symlink_at_path_journaled(&mut device, &superblock, "/dir_alias/new_link", "/target")
            .unwrap();

        assert_eq!(read_symlink_at_path(&mut device, &superblock, "/dir/new_link").unwrap(), "/target");
        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        assert_eq!(allocator_after.allocated_blocks(), allocated_before + 2);
        assert_eq!(load_inode_table(&mut device, &superblock).unwrap().len(), inode_count_before + 2);
        assert_unique_ownership(&mut device, &superblock);
        check_device(&mut device).unwrap();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
        assert_eq!(recover_journal_and_checkpoint(&mut device, superblock).unwrap(), RecoveryReport::default());
    }

    assert!(committed_crash_states > 0);
}

#[test]
fn unlink_symlink_recovers_committed_parent_symlink_before_resolution() {
    let (mut probe, superblock) = setup_with_victim();
    probe.arm(None);
    create_symlink_journaled(&mut probe, &superblock, 1, "dir_alias", "/dir").unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock) = setup_with_victim();
        let allocated_before = load_allocator(&mut device, &superblock).unwrap().allocated_blocks();
        let inode_count_before = load_inode_table(&mut device, &superblock).unwrap().len();

        device.arm(Some(crash_at));
        if create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").is_ok() {
            continue;
        }
        device.reboot();
        if !has_commit(&load_journal_image(&mut device, superblock).unwrap()) {
            continue;
        }
        committed_crash_states += 1;

        unlink_symlink_at_path_journaled(&mut device, &superblock, "/dir_alias/victim").unwrap();

        assert!(resolve_path_without_following_final_symlink(&mut device, &superblock, "/dir/victim").is_err());
        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        assert_eq!(allocator_after.allocated_blocks(), allocated_before);
        assert_eq!(load_inode_table(&mut device, &superblock).unwrap().len(), inode_count_before);
        assert_unique_ownership(&mut device, &superblock);
        check_device(&mut device).unwrap();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
        assert_eq!(recover_journal_and_checkpoint(&mut device, superblock).unwrap(), RecoveryReport::default());
    }

    assert!(committed_crash_states > 0);
}
