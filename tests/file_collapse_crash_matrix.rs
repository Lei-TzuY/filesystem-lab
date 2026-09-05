mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::file_collapse::collapse_file_block_range_journaled;
use filesystem_lab::file_data::read_file_block;
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
    for (index, block) in blocks.iter().enumerate() {
        device
            .write_block(*block, &[0x10 + index as u8; BLOCK_SIZE])
            .unwrap();
    }
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks)
}

fn inode_blocks(device: &mut CrashDevice, superblock: &Superblock) -> Vec<u64> {
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap()
        .blocks
}

fn assert_namespace(device: &mut CrashDevice, superblock: &Superblock) {
    let entries = load_directory_table(device, superblock).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].parent, 1);
    assert_eq!(entries[0].target, 2);
    assert_eq!(entries[0].name, "file");
}

fn assert_old(device: &mut CrashDevice, superblock: &Superblock, blocks: [u64; 5]) {
    assert_eq!(inode_blocks(device, superblock), blocks);
    let allocator = load_allocator(device, superblock).unwrap();
    for block in blocks {
        assert!(allocator.is_owned(block).unwrap());
    }
    assert_namespace(device, superblock);
}

fn assert_new(device: &mut CrashDevice, superblock: &Superblock, blocks: [u64; 5]) {
    assert_eq!(
        inode_blocks(device, superblock),
        vec![blocks[0], blocks[1], blocks[4]]
    );
    let allocator = load_allocator(device, superblock).unwrap();
    assert!(allocator.is_owned(blocks[0]).unwrap());
    assert!(allocator.is_owned(blocks[1]).unwrap());
    assert!(!allocator.is_owned(blocks[2]).unwrap());
    assert!(!allocator.is_owned(blocks[3]).unwrap());
    assert!(allocator.is_owned(blocks[4]).unwrap());
    assert_eq!(
        read_file_block(device, superblock, 2, 2).unwrap(),
        [0x14; BLOCK_SIZE]
    );
    assert_namespace(device, superblock);
}

#[test]
fn collapse_releases_range_and_shifts_suffix_atomically() {
    let (mut device, superblock, blocks) = setup();
    let (released, report) =
        collapse_file_block_range_journaled(&mut device, &superblock, 2, 2, 2).unwrap();
    assert_eq!(released, vec![blocks[2], blocks[3]]);
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 2);
    assert_new(&mut device, &superblock, blocks);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn empty_or_out_of_bounds_range_is_rejected_before_publication() {
    let (mut device, superblock, blocks) = setup();
    let empty = collapse_file_block_range_journaled(&mut device, &superblock, 2, 1, 0).unwrap_err();
    assert_eq!(empty.kind(), io::ErrorKind::InvalidInput);
    let outside =
        collapse_file_block_range_journaled(&mut device, &superblock, 2, 4, 2).unwrap_err();
    assert_eq!(outside.kind(), io::ErrorKind::InvalidInput);
    assert_old(&mut device, &superblock, blocks);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn allocator_disagreement_anywhere_in_range_is_rejected_before_publication() {
    let (mut device, superblock, blocks) = setup();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    allocator.free(blocks[3]).unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    device.flush().unwrap();

    let error = collapse_file_block_range_journaled(&mut device, &superblock, 2, 2, 2).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(inode_blocks(&mut device, &superblock), blocks);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_collapse_mutation_crash_point_is_old_or_recoverable_new_state() {
    let (mut probe, superblock, blocks) = setup();
    probe.arm(None);
    let (_, report) =
        collapse_file_block_range_journaled(&mut probe, &superblock, 2, 2, 2).unwrap();
    assert_eq!(report.home_writes, 2);
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 6);
    assert_new(&mut probe, &superblock, blocks);

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, blocks) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            collapse_file_block_range_journaled(&mut device, &superblock, 2, 2, 2)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();

        let allocator = load_allocator(&mut device, &superblock).unwrap();
        let raw_blocks = inode_blocks(&mut device, &superblock);
        let removed_owned = [blocks[2], blocks[3]]
            .into_iter()
            .map(|block| allocator.is_owned(block).unwrap())
            .collect::<Vec<_>>();
        let raw_is_old = raw_blocks == blocks && removed_owned == vec![true, true];
        let raw_is_new = raw_blocks == vec![blocks[0], blocks[1], blocks[4]]
            && removed_owned == vec![false, false];
        if raw_is_old || raw_is_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(
                check_device(&mut device).is_err(),
                "crash point {crash_at} exposed mixed metadata accepted by fsck"
            );
        }

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_old(&mut device, &superblock, blocks);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(recovery.home_writes, 2);
            assert_new(&mut device, &superblock, blocks);
        }
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());

        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second, RecoveryReport::default());
    }
}
