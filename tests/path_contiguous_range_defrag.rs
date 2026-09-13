mod support;

use std::collections::HashSet;
use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::BLOCK_SIZE;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::file_append_batch::append_file_blocks_journaled;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_contiguous_defrag::defragment_file_range_contiguous_at_path_journaled;
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 12;
const DATA: [[u8; BLOCK_SIZE]; 4] = [
    [0x11; BLOCK_SIZE],
    [0x22; BLOCK_SIZE],
    [0x33; BLOCK_SIZE],
    [0x44; BLOCK_SIZE],
];
const BLOCKER: [u8; BLOCK_SIZE] = [0xee; BLOCK_SIZE];

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

fn setup_fragmented_range() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(224);
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
        &[entry(1, 2, "file"), entry(1, 3, "blocker")],
    )
    .unwrap();

    append_file_blocks_journaled(&mut device, &superblock, 2, &DATA[0..1]).unwrap();
    append_file_blocks_journaled(&mut device, &superblock, 3, &[BLOCKER]).unwrap();
    append_file_blocks_journaled(&mut device, &superblock, 2, &DATA[1..2]).unwrap();
    append_file_blocks_journaled(&mut device, &superblock, 3, &[BLOCKER]).unwrap();
    append_file_blocks_journaled(&mut device, &superblock, 2, &DATA[2..3]).unwrap();
    append_file_blocks_journaled(&mut device, &superblock, 2, &DATA[3..4]).unwrap();

    let file = load_inode_table(&mut device, &superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap();
    assert_ne!(file.blocks[2], file.blocks[1] + 1);
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn file_blocks(device: &mut CrashDevice, superblock: &Superblock) -> Vec<u64> {
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap()
        .blocks
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

fn assert_data_preserved(device: &mut CrashDevice, superblock: &Superblock) {
    for (logical, image) in DATA.iter().enumerate() {
        assert_eq!(
            read_file_range_at_path(
                device,
                superblock,
                "/file",
                logical,
                0,
                BLOCK_SIZE,
            )
            .unwrap(),
            *image
        );
    }
}

#[test]
fn defragments_only_the_selected_fragmented_range() {
    let (mut device, superblock) = setup_fragmented_range();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let before = file_blocks(&mut device, &superblock);

    let (new_range, report) = defragment_file_range_contiguous_at_path_journaled(
        &mut device,
        &superblock,
        "/file",
        1,
        2,
    )
    .unwrap();

    let after = file_blocks(&mut device, &superblock);
    assert_eq!(new_range.len(), 2);
    assert_eq!(new_range[1], new_range[0] + 1);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[3], before[3]);
    assert_eq!(&after[1..3], new_range.as_slice());
    assert_ne!(&after[1..3], &before[1..3]);
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        allocator_before.allocated_blocks()
    );
    for block in &before[1..3] {
        assert!(!load_allocator(&mut device, &superblock)
            .unwrap()
            .is_owned(*block)
            .unwrap());
    }
    assert_data_preserved(&mut device, &superblock);
    assert_eq!(report.committed_transactions, 1);
    assert_unique_file_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn contiguous_selected_range_is_an_idempotent_noop() {
    let (mut device, superblock) = setup_fragmented_range();
    let before = file_blocks(&mut device, &superblock);
    assert_eq!(before[3], before[2] + 1);
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();

    let (range, report) = defragment_file_range_contiguous_at_path_journaled(
        &mut device,
        &superblock,
        "/file",
        2,
        2,
    )
    .unwrap();

    assert_eq!(range, before[2..4]);
    assert_eq!(file_blocks(&mut device, &superblock), before);
    assert_eq!(report, RecoveryReport::default());
    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    check_device(&mut device).unwrap();
}

#[test]
fn every_range_defragmentation_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup_fragmented_range();
    probe.arm(None);
    defragment_file_range_contiguous_at_path_journaled(
        &mut probe,
        &superblock,
        "/file",
        1,
        2,
    )
    .unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup_fragmented_range();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();
        let before = inodes_before
            .iter()
            .find(|inode| inode.id == 2)
            .unwrap()
            .blocks
            .clone();

        device.arm(Some(crash_at));
        assert_eq!(
            defragment_file_range_contiguous_at_path_journaled(
                &mut device,
                &superblock,
                "/file",
                1,
                2,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt range defragmentation"
        );
        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        let after_inodes = load_inode_table(&mut device, &superblock).unwrap();
        let after = after_inodes
            .iter()
            .find(|inode| inode.id == 2)
            .unwrap()
            .blocks
            .clone();
        if after == before {
            assert_eq!(allocator_after, allocator_before);
            assert_eq!(after_inodes, inodes_before);
        } else {
            assert_eq!(after.len(), before.len());
            assert_eq!(after[0], before[0]);
            assert_eq!(after[3], before[3]);
            assert_eq!(after[2], after[1] + 1);
            assert_eq!(
                allocator_after.allocated_blocks(),
                allocator_before.allocated_blocks()
            );
            for block in &before[1..3] {
                assert!(!allocator_after.is_owned(*block).unwrap());
            }
            assert_data_preserved(&mut device, &superblock);
        }

        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            entries_before
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
