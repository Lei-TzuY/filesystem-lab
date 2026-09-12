mod support;

use std::collections::HashSet;
use std::io;

use filesystem_lab::allocation::BlockAllocator;
use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::BLOCK_SIZE;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::file_append_contiguous::append_file_blocks_contiguous_journaled;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_clone_append::PathCloneAppendRange;
use filesystem_lab::path_contiguous_clone_append::clone_file_blocks_contiguous_append_at_path_journaled as clone_append;
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 12;
const SOURCE: [[u8; BLOCK_SIZE]; 3] = [[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE], [0x33; BLOCK_SIZE]];
const DESTINATION: [[u8; BLOCK_SIZE]; 2] = [[0xa1; BLOCK_SIZE], [0xb2; BLOCK_SIZE]];

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
        &[entry(1, 2, "source"), entry(1, 3, "destination")],
    )
    .unwrap();
    append_file_blocks_contiguous_journaled(&mut device, &superblock, 2, &SOURCE).unwrap();
    append_file_blocks_contiguous_journaled(&mut device, &superblock, 3, &DESTINATION).unwrap();
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
            assert!(seen.insert(*block), "duplicate physical block reference {block}");
            assert!(allocator.is_owned(*block).unwrap());
        }
    }
}

fn assert_complete_state(
    device: &mut CrashDevice,
    superblock: &Superblock,
    allocator_before: &BlockAllocator,
    source_before: &PersistedInode,
    destination_before: &PersistedInode,
    destination_after: &PersistedInode,
) {
    assert_eq!(destination_after.blocks.len(), destination_before.blocks.len() + 2);
    assert_eq!(
        &destination_after.blocks[..destination_before.blocks.len()],
        destination_before.blocks.as_slice()
    );
    let appended = &destination_after.blocks[destination_before.blocks.len()..];
    assert_eq!(appended[1], appended[0] + 1);
    assert!(appended
        .iter()
        .all(|block| !source_before.blocks.contains(block)));
    let allocator_after = load_allocator(device, superblock).unwrap();
    assert_eq!(
        allocator_after.allocated_blocks(),
        allocator_before.allocated_blocks() + 2
    );
    assert_eq!(
        read_file_range_at_path(device, superblock, "/destination", 2, 0, BLOCK_SIZE).unwrap(),
        SOURCE[1]
    );
    assert_eq!(
        read_file_range_at_path(device, superblock, "/destination", 3, 0, BLOCK_SIZE).unwrap(),
        SOURCE[2]
    );
}

fn run_clone_append(device: &mut CrashDevice, superblock: &Superblock) -> io::Result<(Vec<u64>, RecoveryReport)> {
    clone_append(
        device,
        superblock,
        PathCloneAppendRange {
            path: "/source",
            start: 1,
            block_count: 2,
        },
        "/destination",
    )
}

fn assert_recovered_crash_state(mut device: CrashDevice, superblock: Superblock) {
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();
    let source_before = inodes_before.iter().find(|inode| inode.id == 2).unwrap().clone();
    let destination_before = inodes_before.iter().find(|inode| inode.id == 3).unwrap().clone();

    device.reboot();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    let inodes_after = load_inode_table(&mut device, &superblock).unwrap();
    let source_after = inodes_after.iter().find(|inode| inode.id == 2).unwrap();
    let destination_after = inodes_after.iter().find(|inode| inode.id == 3).unwrap();
    assert_eq!(source_after, &source_before);
    if destination_after.blocks == destination_before.blocks {
        assert_eq!(load_allocator(&mut device, &superblock).unwrap(), allocator_before);
        assert_eq!(inodes_after, inodes_before);
    } else {
        assert_complete_state(
            &mut device,
            &superblock,
            &allocator_before,
            &source_before,
            &destination_before,
            destination_after,
        );
    }
    assert_eq!(load_directory_table(&mut device, &superblock).unwrap(), entries_before);
    assert_unique_file_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
    assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
    assert_eq!(
        recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
        RecoveryReport::default()
    );
}

#[test]
fn appends_independent_contiguous_source_copies() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let source_before = inodes_before.iter().find(|inode| inode.id == 2).unwrap().clone();
    let destination_before = inodes_before.iter().find(|inode| inode.id == 3).unwrap().clone();

    let (blocks, _) = run_clone_append(&mut device, &superblock).unwrap();

    assert_eq!(blocks.len(), 2);
    let inodes_after = load_inode_table(&mut device, &superblock).unwrap();
    let destination_after = inodes_after.iter().find(|inode| inode.id == 3).unwrap();
    assert_eq!(
        inodes_after.iter().find(|inode| inode.id == 2).unwrap(),
        &source_before
    );
    assert_complete_state(
        &mut device,
        &superblock,
        &allocator_before,
        &source_before,
        &destination_before,
        destination_after,
    );
    assert_unique_file_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_empty_clone_range_without_persistent_change() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    assert_eq!(
        clone_append(
            &mut device,
            &superblock,
            PathCloneAppendRange {
                path: "/source",
                start: 0,
                block_count: 0,
            },
            "/destination",
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(load_allocator(&mut device, &superblock).unwrap(), allocator_before);
    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes_before);
    check_device(&mut device).unwrap();
}

#[test]
fn every_clone_append_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    run_clone_append(&mut probe, &superblock).unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            run_clone_append(&mut device, &superblock).unwrap_err().kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt contiguous clone append"
        );
        assert_recovered_crash_state(device, superblock);
    }
}
