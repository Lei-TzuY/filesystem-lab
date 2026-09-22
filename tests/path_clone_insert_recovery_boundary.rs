mod support;

use std::collections::HashSet;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::BLOCK_SIZE;
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
use filesystem_lab::path_append::append_file_blocks_at_path_journaled;
use filesystem_lab::path_clone_insert::{
    clone_file_blocks_insert_at_path_journaled, PathCloneInsertRange,
};
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 10;

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

fn setup_without_source_alias() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(128);
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
        &[entry(1, 2, "src"), entry(1, 3, "dst")],
    )
    .unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/src",
        &[[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE]],
    )
    .unwrap();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/dst",
        &[[0x33; BLOCK_SIZE], [0x44; BLOCK_SIZE]],
    )
    .unwrap();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn has_commit(entries: &[JournalEntry]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry, JournalEntry::Commit { .. }))
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
fn clone_insert_recovers_committed_source_symlink_before_resolving_it() {
    let (mut probe, superblock) = setup_without_source_alias();
    probe.arm(None);
    create_symlink_journaled(&mut probe, &superblock, 1, "src_alias", "/src").unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock) = setup_without_source_alias();
        let allocated_before = load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        if create_symlink_journaled(&mut device, &superblock, 1, "src_alias", "/src").is_ok() {
            continue;
        }
        device.reboot();
        let durable_journal = load_journal_image(&mut device, superblock).unwrap();
        if !has_commit(&durable_journal) {
            continue;
        }
        committed_crash_states += 1;

        let (new_blocks, _) = clone_file_blocks_insert_at_path_journaled(
            &mut device,
            &superblock,
            PathCloneInsertRange {
                path: "/src_alias",
                start: 0,
                block_count: 2,
            },
            "/dst",
            1,
        )
        .unwrap();

        assert_eq!(new_blocks.len(), 2);
        assert_eq!(
            read_file_range_at_path(&mut device, &superblock, "/dst", 0, 0, 1).unwrap(),
            vec![0x33]
        );
        assert_eq!(
            read_file_range_at_path(&mut device, &superblock, "/dst", 1, 0, 1).unwrap(),
            vec![0x11]
        );
        assert_eq!(
            read_file_range_at_path(&mut device, &superblock, "/dst", 2, 0, 1).unwrap(),
            vec![0x22]
        );
        assert_eq!(
            read_file_range_at_path(&mut device, &superblock, "/dst", 3, 0, 1).unwrap(),
            vec![0x44]
        );
        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        assert_eq!(allocator_after.allocated_blocks(), allocated_before + 3);
        let inodes_after = load_inode_table(&mut device, &superblock).unwrap();
        assert_eq!(inodes_after.len(), inodes_before.len() + 1);
        assert_unique_ownership(&mut device, &superblock);
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
