mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::BlockDevice;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_transfer::move_file_block_range_journaled;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn setup() -> (CrashDevice, Superblock, [u64; 5]) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let blocks = [
        allocator.allocate().unwrap(),
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
            PersistedInode {
                id: 1,
                kind: InodeKind::Directory,
                blocks: Vec::new(),
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::File,
                blocks: blocks.to_vec(),
            },
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "file".to_owned(),
        }],
    )
    .unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks)
}

fn blocks_for(device: &mut CrashDevice, superblock: &Superblock) -> Vec<u64> {
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap()
        .blocks
}

fn assert_owned(device: &mut CrashDevice, superblock: &Superblock, blocks: [u64; 5]) {
    let allocator = load_allocator(device, superblock).unwrap();
    for block in blocks {
        assert!(allocator.is_owned(block).unwrap());
    }
}

#[test]
fn move_reorders_references_without_changing_ownership() {
    let (mut device, superblock, blocks) = setup();
    let (moved, report) =
        move_file_block_range_journaled(&mut device, &superblock, 2, 1, 2, 3).unwrap();
    assert_eq!(moved, vec![blocks[1], blocks[2]]);
    assert_eq!(
        blocks_for(&mut device, &superblock),
        vec![blocks[0], blocks[3], blocks[4], blocks[1], blocks[2]]
    );
    assert_eq!(report.committed_transactions, 1);
    assert_owned(&mut device, &superblock, blocks);
    check_device(&mut device).unwrap();
}

#[test]
fn invalid_moves_are_rejected_before_publication() {
    let (mut device, superblock, blocks) = setup();
    for error in [
        move_file_block_range_journaled(&mut device, &superblock, 2, 0, 0, 1).unwrap_err(),
        move_file_block_range_journaled(&mut device, &superblock, 2, 4, 2, 0).unwrap_err(),
        move_file_block_range_journaled(&mut device, &superblock, 2, 1, 2, 4).unwrap_err(),
        move_file_block_range_journaled(&mut device, &superblock, 2, 1, 2, 1).unwrap_err(),
    ] {
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
    assert_eq!(blocks_for(&mut device, &superblock), blocks);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn allocator_disagreement_is_rejected_before_publication() {
    let (mut device, superblock, blocks) = setup();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    allocator.free(blocks[2]).unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    device.flush().unwrap();
    let error = move_file_block_range_journaled(&mut device, &superblock, 2, 1, 2, 3).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(blocks_for(&mut device, &superblock), blocks);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_move_mutation_crash_point_is_old_or_recoverable_new_state() {
    let (mut probe, superblock, blocks) = setup();
    probe.arm(None);
    let (_, report) = move_file_block_range_journaled(&mut probe, &superblock, 2, 1, 2, 3).unwrap();
    let expected_home_writes = report.home_writes;
    let operations = probe.operations();
    let new_blocks = vec![blocks[0], blocks[3], blocks[4], blocks[1], blocks[2]];

    for crash_at in 0..operations {
        let (mut device, superblock, blocks) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            move_file_block_range_journaled(&mut device, &superblock, 2, 1, 2, 3)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();

        let raw = blocks_for(&mut device, &superblock);
        assert!(
            raw == blocks || raw == new_blocks,
            "crash point {crash_at} exposed partial reorder"
        );
        check_device(&mut device).unwrap();

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_eq!(blocks_for(&mut device, &superblock), blocks);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(recovery.home_writes, expected_home_writes);
            assert_eq!(blocks_for(&mut device, &superblock), new_blocks);
        }
        assert_owned(&mut device, &superblock, blocks);
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
