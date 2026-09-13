mod support;

use std::collections::HashSet;
use std::io;

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
use filesystem_lab::path_contiguous_replace::replace_with_zeroed_blocks_contiguous_at_path_journaled;
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 12;
const INITIAL: [[u8; BLOCK_SIZE]; 4] = [
    [0x11; BLOCK_SIZE],
    [0x22; BLOCK_SIZE],
    [0x33; BLOCK_SIZE],
    [0x44; BLOCK_SIZE],
];

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
    let mut device = CrashDevice::new(192);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "dir"), entry(2, 3, "file")],
    )
    .unwrap();
    append_file_blocks_contiguous_journaled(&mut device, &superblock, 3, &INITIAL).unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/dir/file").unwrap();
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
fn replaces_an_interior_range_with_fresh_contiguous_zeroed_blocks() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let file_before = load_inode_table(&mut device, &superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 3)
        .unwrap();
    let displaced_expected = file_before.blocks[1..3].to_vec();

    let (new_blocks, displaced, _) = replace_with_zeroed_blocks_contiguous_at_path_journaled(
        &mut device,
        &superblock,
        "/file_alias",
        1,
        2,
    )
    .unwrap();

    assert_eq!(displaced, displaced_expected);
    assert_eq!(new_blocks.len(), 2);
    assert!(new_blocks.windows(2).all(|pair| pair[1] == pair[0] + 1));
    assert!(new_blocks.iter().all(|block| !displaced.contains(block)));

    let allocator_after = load_allocator(&mut device, &superblock).unwrap();
    assert_eq!(
        allocator_after.allocated_blocks(),
        allocator_before.allocated_blocks()
    );
    for block in displaced_expected {
        assert!(!allocator_after.is_owned(block).unwrap());
    }
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 0, 0, BLOCK_SIZE).unwrap(),
        INITIAL[0]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 1, 0, BLOCK_SIZE).unwrap(),
        [0_u8; BLOCK_SIZE]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 2, 0, BLOCK_SIZE).unwrap(),
        [0_u8; BLOCK_SIZE]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 3, 0, BLOCK_SIZE).unwrap(),
        INITIAL[3]
    );
    assert_unique_file_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn rejects_invalid_zero_replacement_without_persistent_change() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    assert_eq!(
        replace_with_zeroed_blocks_contiguous_at_path_journaled(
            &mut device,
            &superblock,
            "/dir/file",
            1,
            0,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        replace_with_zeroed_blocks_contiguous_at_path_journaled(
            &mut device,
            &superblock,
            "/dir/file",
            3,
            2,
        )
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
    check_device(&mut device).unwrap();
}

#[test]
fn every_zero_replacement_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    replace_with_zeroed_blocks_contiguous_at_path_journaled(
        &mut probe,
        &superblock,
        "/file_alias",
        1,
        2,
    )
    .unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();
        let file_before = inodes_before
            .iter()
            .find(|inode| inode.id == 3)
            .unwrap()
            .clone();
        let displaced = file_before.blocks[1..3].to_vec();

        device.arm(Some(crash_at));
        assert_eq!(
            replace_with_zeroed_blocks_contiguous_at_path_journaled(
                &mut device,
                &superblock,
                "/file_alias",
                1,
                2,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt contiguous zero replacement"
        );
        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        let inodes_after = load_inode_table(&mut device, &superblock).unwrap();
        let file_after = inodes_after.iter().find(|inode| inode.id == 3).unwrap();
        if file_after.blocks == file_before.blocks {
            assert_eq!(allocator_after, allocator_before);
            assert_eq!(inodes_after, inodes_before);
            assert_eq!(
                read_file_range_at_path(&mut device, &superblock, "/dir/file", 1, 0, BLOCK_SIZE,)
                    .unwrap(),
                INITIAL[1]
            );
        } else {
            assert_eq!(file_after.blocks.len(), file_before.blocks.len());
            let replacement_blocks = &file_after.blocks[1..3];
            assert!(replacement_blocks
                .windows(2)
                .all(|pair| pair[1] == pair[0] + 1));
            assert!(replacement_blocks
                .iter()
                .all(|block| !displaced.contains(block)));
            assert_eq!(file_after.blocks[0], file_before.blocks[0]);
            assert_eq!(file_after.blocks[3], file_before.blocks[3]);
            assert_eq!(
                allocator_after.allocated_blocks(),
                allocator_before.allocated_blocks()
            );
            for block in &displaced {
                assert!(!allocator_after.is_owned(*block).unwrap());
            }
            for logical_block in 1..3 {
                assert_eq!(
                    read_file_range_at_path(
                        &mut device,
                        &superblock,
                        "/dir/file",
                        logical_block,
                        0,
                        BLOCK_SIZE,
                    )
                    .unwrap(),
                    [0_u8; BLOCK_SIZE]
                );
            }
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
