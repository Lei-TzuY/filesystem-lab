mod support;

use std::collections::HashSet;
use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
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
use filesystem_lab::path_remove::remove_file_block_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

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

fn setup_without_file_alias() -> (CrashDevice, Superblock, [u64; 4]) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, 10).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let blocks = [
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
    ];
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory, Vec::new()),
            inode(2, InodeKind::Directory, Vec::new()),
            inode(3, InodeKind::File, blocks.to_vec()),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "dir"), entry(2, 3, "file")],
    )
    .unwrap();
    for (index, block) in blocks.iter().enumerate() {
        let byte = 0x30 + u8::try_from(index).unwrap();
        device.write_block(*block, &[byte; BLOCK_SIZE]).unwrap();
    }
    device.flush().unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dangling", "/missing").unwrap();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks)
}

fn setup() -> (CrashDevice, Superblock, [u64; 4]) {
    let (mut device, superblock, blocks) = setup_without_file_alias();
    create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/dir/file").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks)
}

fn file_blocks(device: &mut CrashDevice, superblock: &Superblock) -> Vec<u64> {
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 3)
        .unwrap()
        .blocks
}

fn has_commit(entries: &[JournalEntry]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry, JournalEntry::Commit { .. }))
}

fn assert_unique_file_ownership(device: &mut CrashDevice, superblock: &Superblock) {
    let allocator = load_allocator(device, superblock).unwrap();
    allocator.validate().unwrap();
    let inodes = load_inode_table(device, superblock).unwrap();
    let mut seen = HashSet::new();
    for inode in inodes.iter().filter(|inode| inode.kind == InodeKind::File) {
        for block in &inode.blocks {
            assert!(
                seen.insert(*block),
                "duplicate physical block reference {block}"
            );
            assert!(allocator.is_owned(*block).unwrap());
        }
    }
}

fn remove_alias(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> io::Result<(u64, RecoveryReport)> {
    remove_file_block_at_path_journaled(device, superblock, "/file_alias", 1)
}

#[test]
fn removes_blocks_through_direct_and_symlink_paths() {
    for path in ["/dir/file", "/dir_alias/file", "/file_alias"] {
        let (mut device, superblock, blocks) = setup();
        let (released, report) =
            remove_file_block_at_path_journaled(&mut device, &superblock, path, 1).unwrap();
        assert_eq!(released, blocks[1]);
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(
            file_blocks(&mut device, &superblock),
            vec![blocks[0], blocks[2], blocks[3]]
        );
        let allocator = load_allocator(&mut device, &superblock).unwrap();
        assert!(!allocator.is_owned(blocks[1]).unwrap());
        for block in [blocks[0], blocks[2], blocks[3]] {
            assert!(allocator.is_owned(block).unwrap());
        }
        assert_unique_file_ownership(&mut device, &superblock);
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn rejects_invalid_pathname_removals_without_publication() {
    let (mut device, superblock, _) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let directory_before = load_directory_table(&mut device, &superblock).unwrap();

    assert_eq!(
        remove_file_block_at_path_journaled(&mut device, &superblock, "/dir", 0)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        remove_file_block_at_path_journaled(&mut device, &superblock, "/dangling", 0)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        remove_file_block_at_path_journaled(&mut device, &superblock, "/missing", 0)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        remove_file_block_at_path_journaled(&mut device, &superblock, "/dir/file", 4)
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
        directory_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn remove_recovers_committed_final_symlink_before_resolving_it() {
    let (mut probe, superblock, _) = setup_without_file_alias();
    probe.arm(None);
    create_symlink_journaled(&mut probe, &superblock, 1, "file_alias", "/dir/file").unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock, blocks) = setup_without_file_alias();
        let allocated_before = load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        if create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/dir/file").is_ok()
        {
            continue;
        }
        device.reboot();
        let durable_journal = load_journal_image(&mut device, superblock).unwrap();
        if !has_commit(&durable_journal) {
            continue;
        }
        committed_crash_states += 1;

        let (released, _) = remove_alias(&mut device, &superblock).unwrap();
        assert_eq!(released, blocks[1]);
        assert_eq!(
            file_blocks(&mut device, &superblock),
            vec![blocks[0], blocks[2], blocks[3]]
        );

        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        assert_eq!(allocator_after.allocated_blocks(), allocated_before);
        assert!(!allocator_after.is_owned(blocks[1]).unwrap());
        let inodes_after = load_inode_table(&mut device, &superblock).unwrap();
        assert_eq!(inodes_after.len(), inodes_before.len() + 1);
        assert_unique_file_ownership(&mut device, &superblock);
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
fn every_pathname_remove_crash_point_recovers_old_or_complete_new_state() {
    let (mut expected, superblock, _) = setup();
    let allocator_old = load_allocator(&mut expected, &superblock).unwrap();
    let inodes_old = load_inode_table(&mut expected, &superblock).unwrap();
    let directory_old = load_directory_table(&mut expected, &superblock).unwrap();
    remove_alias(&mut expected, &superblock).unwrap();
    let allocator_new = load_allocator(&mut expected, &superblock).unwrap();
    let inodes_new = load_inode_table(&mut expected, &superblock).unwrap();

    let (mut probe, superblock, _) = setup();
    probe.arm(None);
    remove_alias(&mut probe, &superblock).unwrap();
    let operations = probe.operations();
    assert!(operations >= 6);

    for crash_at in 0..operations {
        let (mut device, superblock, _) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            remove_alias(&mut device, &superblock).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let allocator = load_allocator(&mut device, &superblock).unwrap();
        let inodes = load_inode_table(&mut device, &superblock).unwrap();
        assert!(
            (allocator == allocator_old && inodes == inodes_old)
                || (allocator == allocator_new && inodes == inodes_new),
            "crash point {crash_at} recovered a mixed allocator/inode state"
        );
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            directory_old
        );
        assert_unique_file_ownership(&mut device, &superblock);
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
