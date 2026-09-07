mod support;

use std::collections::HashSet;
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
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 10;
const APPEND: [[u8; BLOCK_SIZE]; 2] = [[0xa1; BLOCK_SIZE], [0xb2; BLOCK_SIZE]];

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
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/dir/file").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dangling", "/missing").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn append_through_final_symlink(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    append_file_blocks_at_path_journaled(device, superblock, "/file_alias", &APPEND)
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
fn appends_complete_blocks_through_direct_and_symlink_paths() {
    let (mut device, superblock) = setup();

    let (first, _) = append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/dir/file",
        &[[0x11; BLOCK_SIZE]],
    )
    .unwrap();
    let (next, _) = append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/dir_alias/file",
        &[[0x22; BLOCK_SIZE]],
    )
    .unwrap();
    let (last, _) = append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/file_alias",
        &[[0x33; BLOCK_SIZE]],
    )
    .unwrap();

    assert_eq!(first.len(), 1);
    assert_eq!(next.len(), 1);
    assert_eq!(last.len(), 1);
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 0, 0, 1).unwrap(),
        vec![0x11]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 1, 0, 1).unwrap(),
        vec![0x22]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 2, 0, 1).unwrap(),
        vec![0x33]
    );
    assert_unique_file_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
}

#[test]
fn propagates_path_and_append_validation_before_publication() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    assert_eq!(
        append_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/dir",
            &[[0x44; BLOCK_SIZE]],
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        append_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/dangling",
            &[[0x55; BLOCK_SIZE]],
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        append_file_blocks_at_path_journaled(&mut device, &superblock, "/dir/file", &[])
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
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_pathname_append_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    append_through_final_symlink(&mut probe, &superblock).unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let directory_before = load_directory_table(&mut device, &superblock).unwrap();
        let file_before = inodes_before
            .iter()
            .find(|inode| inode.id == 3)
            .unwrap()
            .clone();

        device.arm(Some(crash_at));
        assert_eq!(
            append_through_final_symlink(&mut device, &superblock)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        let _ = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        let inodes_after = load_inode_table(&mut device, &superblock).unwrap();
        let file_after = inodes_after.iter().find(|inode| inode.id == 3).unwrap();
        if file_after.blocks.len() == file_before.blocks.len() {
            assert_eq!(allocator_after, allocator_before);
            assert_eq!(inodes_after, inodes_before);
        } else {
            assert_eq!(file_after.blocks.len(), file_before.blocks.len() + 2);
            assert_eq!(
                allocator_after.allocated_blocks(),
                allocator_before.allocated_blocks() + 2
            );
            assert_eq!(
                read_file_range_at_path(
                    &mut device,
                    &superblock,
                    "/dir/file",
                    file_before.blocks.len(),
                    0,
                    1,
                )
                .unwrap(),
                vec![0xa1]
            );
            assert_eq!(
                read_file_range_at_path(
                    &mut device,
                    &superblock,
                    "/dir/file",
                    file_before.blocks.len() + 1,
                    0,
                    1,
                )
                .unwrap(),
                vec![0xb2]
            );
        }

        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            directory_before
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
