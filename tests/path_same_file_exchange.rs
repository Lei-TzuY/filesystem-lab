mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::BLOCK_SIZE;
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
use filesystem_lab::path_append::append_file_blocks_at_path_journaled;
use filesystem_lab::path_exchange::{
    exchange_same_file_block_ranges_at_path_journaled, PathVariableFileBlockRange,
};
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

fn range(path: &str, start: usize, block_count: usize) -> PathVariableFileBlockRange<'_> {
    PathVariableFileBlockRange {
        path,
        start,
        block_count,
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
            entry(2, 3, "left"),
            entry(2, 4, "right"),
        ],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "left_alias", "/dir/left").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "right_alias", "/dir/right").unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/dir/left",
        &[
            [0x11; BLOCK_SIZE],
            [0x22; BLOCK_SIZE],
            [0x33; BLOCK_SIZE],
            [0x44; BLOCK_SIZE],
        ],
    )
    .unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/dir/right",
        &[[0x55; BLOCK_SIZE]],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn exchange_disjoint_ranges(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> io::Result<RecoveryReport> {
    exchange_same_file_block_ranges_at_path_journaled(
        device,
        superblock,
        range("/left_alias", 0, 1),
        range("/dir_alias/left", 2, 2),
    )
}

#[test]
fn exchanges_different_length_ranges_through_paths_resolving_to_one_inode() {
    let (mut device, superblock) = setup();
    let before = load_inode_table(&mut device, &superblock).unwrap();
    let left_before = before.iter().find(|inode| inode.id == 3).unwrap();
    let expected = vec![
        left_before.blocks[2],
        left_before.blocks[3],
        left_before.blocks[1],
        left_before.blocks[0],
    ];

    exchange_disjoint_ranges(&mut device, &superblock).unwrap();

    let after = load_inode_table(&mut device, &superblock).unwrap();
    let left_after = after.iter().find(|inode| inode.id == 3).unwrap();
    assert_eq!(left_after.blocks, expected);
    assert_eq!(left_after.blocks.len(), left_before.blocks.len());
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_invalid_same_file_path_exchange_without_publication() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let directory_before = load_directory_table(&mut device, &superblock).unwrap();

    for result in [
        exchange_same_file_block_ranges_at_path_journaled(
            &mut device,
            &superblock,
            range("/left_alias", 0, 1),
            range("/right_alias", 0, 1),
        ),
        exchange_same_file_block_ranges_at_path_journaled(
            &mut device,
            &superblock,
            range("/left_alias", 0, 2),
            range("/dir/left", 1, 1),
        ),
        exchange_same_file_block_ranges_at_path_journaled(
            &mut device,
            &superblock,
            range("/left_alias", 0, 0),
            range("/dir/left", 2, 1),
        ),
        exchange_same_file_block_ranges_at_path_journaled(
            &mut device,
            &superblock,
            range("/dir", 0, 1),
            range("/dir_alias", 1, 1),
        ),
        exchange_same_file_block_ranges_at_path_journaled(
            &mut device,
            &superblock,
            range("/left_alias", 0, 1),
            range("/dir/left", 4, 1),
        ),
    ] {
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

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
fn every_same_file_path_exchange_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    let allocator_before = load_allocator(&mut probe, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut probe, &superblock).unwrap();
    let directory_before = load_directory_table(&mut probe, &superblock).unwrap();
    probe.arm(None);
    exchange_disjoint_ranges(&mut probe, &superblock).unwrap();
    let operations = probe.operations();
    let inodes_after = load_inode_table(&mut probe, &superblock).unwrap();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            exchange_disjoint_ranges(&mut device, &superblock)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let recovered_inodes = load_inode_table(&mut device, &superblock).unwrap();
        assert!(recovered_inodes == inodes_before || recovered_inodes == inodes_after);
        assert_eq!(
            load_allocator(&mut device, &superblock).unwrap(),
            allocator_before
        );
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            directory_before
        );
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
