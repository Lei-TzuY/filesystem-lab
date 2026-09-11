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
use filesystem_lab::path_grow::resize_file_at_path_to_blocks_journaled;
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::recovery::RecoveryReport;
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

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::File),
            inode(3, InodeKind::Directory),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "file"), entry(1, 3, "dir")],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
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

#[test]
fn resizes_regular_file_in_both_directions() {
    let (mut device, superblock) = setup();
    let allocated_before = load_allocator(&mut device, &superblock)
        .unwrap()
        .allocated_blocks();

    let (grown, grow_report) =
        resize_file_at_path_to_blocks_journaled(&mut device, &superblock, "/file", 3).unwrap();
    assert_eq!(grown.len(), 3);
    assert_eq!(grow_report.committed_transactions, 1);
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        allocated_before + 3
    );
    for index in 0..3 {
        assert_eq!(
            read_file_range_at_path(&mut device, &superblock, "/file", index, 0, 1).unwrap(),
            vec![0]
        );
    }

    let (released, shrink_report) =
        resize_file_at_path_to_blocks_journaled(&mut device, &superblock, "/file", 1).unwrap();
    assert_eq!(released.len(), 2);
    assert_eq!(shrink_report.committed_transactions, 1);
    let file = load_inode_table(&mut device, &superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap();
    assert_eq!(file.blocks.len(), 1);
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        allocated_before + 1
    );
    assert_unique_file_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_equal_size_and_non_file_targets_without_mutation() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let directory_before = load_directory_table(&mut device, &superblock).unwrap();

    assert_eq!(
        resize_file_at_path_to_blocks_journaled(&mut device, &superblock, "/file", 0)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        resize_file_at_path_to_blocks_journaled(&mut device, &superblock, "/dir", 1)
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
fn every_resize_growth_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    resize_file_at_path_to_blocks_journaled(&mut probe, &superblock, "/file", 2).unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocated_before = load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks();
        device.arm(Some(crash_at));
        assert_eq!(
            resize_file_at_path_to_blocks_journaled(&mut device, &superblock, "/file", 2)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        let _ = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let allocator = load_allocator(&mut device, &superblock).unwrap();
        let file = load_inode_table(&mut device, &superblock)
            .unwrap()
            .into_iter()
            .find(|inode| inode.id == 2)
            .unwrap();
        assert!(file.blocks.is_empty() || file.blocks.len() == 2);
        assert_eq!(
            allocator.allocated_blocks(),
            allocated_before + u64::try_from(file.blocks.len()).unwrap()
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
