mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::BlockDevice;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::truncate_tx::truncate_file_last_block_journaled;
use support::CrashDevice;

fn root_inode() -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }
}

fn setup() -> (
    CrashDevice,
    Superblock,
    Vec<u64>,
    Vec<PersistedDirectoryEntry>,
) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let blocks = vec![allocator.allocate().unwrap(), allocator.allocate().unwrap()];
    let inodes = vec![
        root_inode(),
        PersistedInode {
            id: 2,
            kind: InodeKind::File,
            blocks: blocks.clone(),
        },
    ];
    let entries = vec![PersistedDirectoryEntry {
        parent: 1,
        target: 2,
        name: "file".to_owned(),
    }];

    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(&mut device, &superblock, &inodes).unwrap();
    store_directory_table(&mut device, &superblock, &entries).unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks, entries)
}

fn assert_old_state(
    device: &mut CrashDevice,
    superblock: &Superblock,
    blocks: &[u64],
    entries: &[PersistedDirectoryEntry],
) {
    let allocator = load_allocator(device, superblock).unwrap();
    assert!(allocator.is_owned(blocks[0]).unwrap());
    assert!(allocator.is_owned(blocks[1]).unwrap());
    let inodes = load_inode_table(device, superblock).unwrap();
    assert_eq!(
        inodes.iter().find(|inode| inode.id == 2).unwrap().blocks,
        blocks
    );
    assert_eq!(load_directory_table(device, superblock).unwrap(), entries);
}

fn assert_new_state(
    device: &mut CrashDevice,
    superblock: &Superblock,
    blocks: &[u64],
    entries: &[PersistedDirectoryEntry],
) {
    let allocator = load_allocator(device, superblock).unwrap();
    assert!(allocator.is_owned(blocks[0]).unwrap());
    assert!(!allocator.is_owned(blocks[1]).unwrap());
    let inodes = load_inode_table(device, superblock).unwrap();
    assert_eq!(
        inodes.iter().find(|inode| inode.id == 2).unwrap().blocks,
        vec![blocks[0]]
    );
    assert_eq!(load_directory_table(device, superblock).unwrap(), entries);
}

#[test]
fn truncate_last_block_releases_exactly_one_block_and_checkpoints() {
    let (mut device, superblock, blocks, entries) = setup();

    let (released, report) =
        truncate_file_last_block_journaled(&mut device, &superblock, 2).unwrap();

    assert_eq!(released, blocks[1]);
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 2);
    assert_new_state(&mut device, &superblock, &blocks, &entries);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn truncate_last_block_rejects_empty_and_non_file_targets_without_mutation() {
    let (mut device, superblock, blocks, entries) = setup();

    let error = truncate_file_last_block_journaled(&mut device, &superblock, 1).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_old_state(&mut device, &superblock, &blocks, &entries);

    truncate_file_last_block_journaled(&mut device, &superblock, 2).unwrap();
    truncate_file_last_block_journaled(&mut device, &superblock, 2).unwrap();
    let error = truncate_file_last_block_journaled(&mut device, &superblock, 2).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    check_device(&mut device).unwrap();
}

#[test]
fn every_truncate_last_block_mutation_crash_point_recovers_to_old_or_new_state() {
    let (mut probe, superblock, blocks, entries) = setup();
    probe.arm(None);
    truncate_file_last_block_journaled(&mut probe, &superblock, 2).unwrap();
    let mutation_operations = probe.operations();

    assert!(
        mutation_operations >= 6,
        "single-block truncate must cross WAL, two home metadata writes, and checkpoint"
    );
    assert_new_state(&mut probe, &superblock, &blocks, &entries);
    assert!(load_journal_image(&mut probe, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut probe).unwrap();

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, blocks, entries) = setup();
        device.arm(Some(crash_at));

        assert_eq!(
            truncate_file_last_block_journaled(&mut device, &superblock, 2)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt single-block truncate"
        );

        device.reboot();

        let raw_is_old = {
            let allocator = load_allocator(&mut device, &superblock).unwrap();
            let disk_inodes = load_inode_table(&mut device, &superblock).unwrap();
            let file = disk_inodes.iter().find(|inode| inode.id == 2).unwrap();
            allocator.is_owned(blocks[0]).unwrap()
                && allocator.is_owned(blocks[1]).unwrap()
                && file.blocks == blocks
        };
        let raw_is_new = {
            let allocator = load_allocator(&mut device, &superblock).unwrap();
            let disk_inodes = load_inode_table(&mut device, &superblock).unwrap();
            let file = disk_inodes.iter().find(|inode| inode.id == 2).unwrap();
            allocator.is_owned(blocks[0]).unwrap()
                && !allocator.is_owned(blocks[1]).unwrap()
                && file.blocks == vec![blocks[0]]
        };

        if raw_is_old || raw_is_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(
                check_device(&mut device).is_err(),
                "crash point {crash_at} exposed partial truncate metadata accepted by fsck"
            );
        }

        recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        let journal_empty = load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty();
        assert!(journal_empty);

        let final_is_old = {
            let allocator = load_allocator(&mut device, &superblock).unwrap();
            let disk_inodes = load_inode_table(&mut device, &superblock).unwrap();
            let file = disk_inodes.iter().find(|inode| inode.id == 2).unwrap();
            allocator.is_owned(blocks[0]).unwrap()
                && allocator.is_owned(blocks[1]).unwrap()
                && file.blocks == blocks
        };
        if final_is_old {
            assert_old_state(&mut device, &superblock, &blocks, &entries);
        } else {
            assert_new_state(&mut device, &superblock, &blocks, &entries);
        }
        check_device(&mut device).unwrap();

        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }
}
