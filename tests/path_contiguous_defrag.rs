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
use filesystem_lab::path_contiguous_defrag::defragment_file_contiguous_at_path_journaled;
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 12;
const DATA: [[u8; BLOCK_SIZE]; 3] = [[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE], [0x33; BLOCK_SIZE]];
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

fn setup_fragmented() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(192);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::File),
            inode(4, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 2, "dir"),
            entry(2, 3, "file"),
            entry(2, 4, "blocker"),
        ],
    )
    .unwrap();

    append_file_blocks_journaled(&mut device, &superblock, 3, &DATA[0..1]).unwrap();
    append_file_blocks_journaled(&mut device, &superblock, 4, &[BLOCKER]).unwrap();
    append_file_blocks_journaled(&mut device, &superblock, 3, &DATA[1..2]).unwrap();
    append_file_blocks_journaled(&mut device, &superblock, 4, &[BLOCKER]).unwrap();
    append_file_blocks_journaled(&mut device, &superblock, 3, &DATA[2..3]).unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/dir/file").unwrap();

    let file = load_inode_table(&mut device, &superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 3)
        .unwrap();
    assert!(!file.blocks.windows(2).all(|pair| pair[1] == pair[0] + 1));
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
fn defragments_a_fragmented_file_through_a_final_symlink() {
    let (mut device, superblock) = setup_fragmented();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let old_blocks = load_inode_table(&mut device, &superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 3)
        .unwrap()
        .blocks;

    let (new_blocks, report) =
        defragment_file_contiguous_at_path_journaled(&mut device, &superblock, "/file_alias")
            .unwrap();

    assert_eq!(new_blocks.len(), DATA.len());
    assert!(new_blocks.windows(2).all(|pair| pair[1] == pair[0] + 1));
    assert_ne!(new_blocks, old_blocks);
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        allocator_before.allocated_blocks()
    );
    for block in old_blocks {
        assert!(!load_allocator(&mut device, &superblock)
            .unwrap()
            .is_owned(block)
            .unwrap());
    }
    for (logical, image) in DATA.iter().enumerate() {
        assert_eq!(
            read_file_range_at_path(
                &mut device,
                &superblock,
                "/dir/file",
                logical,
                0,
                BLOCK_SIZE,
            )
            .unwrap(),
            *image
        );
    }
    assert_eq!(report.committed_transactions, 1);
    assert_unique_file_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn already_contiguous_file_is_an_idempotent_noop() {
    let mut device = CrashDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[inode(1, InodeKind::Directory), inode(2, InodeKind::File)],
    )
    .unwrap();
    store_directory_table(&mut device, &superblock, &[entry(1, 2, "file")]).unwrap();
    let (blocks, _) = append_file_blocks_journaled(&mut device, &superblock, 2, &DATA).unwrap();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();

    let (after, report) =
        defragment_file_contiguous_at_path_journaled(&mut device, &superblock, "/file").unwrap();

    assert_eq!(after, blocks);
    assert_eq!(report, RecoveryReport::default());
    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    check_device(&mut device).unwrap();
}

#[test]
fn every_defragmentation_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup_fragmented();
    probe.arm(None);
    defragment_file_contiguous_at_path_journaled(&mut probe, &superblock, "/file_alias").unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup_fragmented();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();
        let file_before = inodes_before
            .iter()
            .find(|inode| inode.id == 3)
            .unwrap()
            .clone();

        device.arm(Some(crash_at));
        assert_eq!(
            defragment_file_contiguous_at_path_journaled(&mut device, &superblock, "/file_alias",)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt defragmentation"
        );
        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        let inodes_after = load_inode_table(&mut device, &superblock).unwrap();
        let file_after = inodes_after.iter().find(|inode| inode.id == 3).unwrap();
        if file_after.blocks == file_before.blocks {
            assert_eq!(allocator_after, allocator_before);
            assert_eq!(inodes_after, inodes_before);
        } else {
            assert_eq!(file_after.blocks.len(), file_before.blocks.len());
            assert!(file_after
                .blocks
                .windows(2)
                .all(|pair| pair[1] == pair[0] + 1));
            assert_eq!(
                allocator_after.allocated_blocks(),
                allocator_before.allocated_blocks()
            );
            for old_block in &file_before.blocks {
                assert!(!allocator_after.is_owned(*old_block).unwrap());
            }
            for (logical, image) in DATA.iter().enumerate() {
                assert_eq!(
                    read_file_range_at_path(
                        &mut device,
                        &superblock,
                        "/dir/file",
                        logical,
                        0,
                        BLOCK_SIZE,
                    )
                    .unwrap(),
                    *image
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
