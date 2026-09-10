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
use filesystem_lab::path_clone_create::{
    clone_file_blocks_to_path_journaled, clone_file_to_path_journaled,
};
use filesystem_lab::path_create::{
    create_empty_file_at_path_journaled, create_file_with_blocks_at_path_journaled,
};
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::path_metadata::metadata_at_path;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 18;

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

fn setup() -> (CrashDevice, Superblock, [[u8; BLOCK_SIZE]; 3]) {
    let mut device = CrashDevice::new(192);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::Directory),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "src"), entry(1, 3, "dst")],
    )
    .unwrap();
    let source = [
        [0x19_u8; BLOCK_SIZE],
        [0x2a_u8; BLOCK_SIZE],
        [0x3b_u8; BLOCK_SIZE],
    ];
    create_file_with_blocks_at_path_journaled(&mut device, &superblock, "/src/source", &source)
        .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "src_alias", "/src").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dst_alias", "/dst").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, source)
}

fn assert_unique_ownership(device: &mut CrashDevice, superblock: &Superblock) {
    let allocator = load_allocator(device, superblock).unwrap();
    allocator.validate().unwrap();
    let inodes = load_inode_table(device, superblock).unwrap();
    let mut seen = HashSet::new();
    for inode in &inodes {
        for block in &inode.blocks {
            assert!(
                seen.insert(*block),
                "duplicate physical block reference {block}"
            );
            assert!(allocator.is_owned(*block).unwrap());
        }
    }
}

fn source_blocks(device: &mut CrashDevice, superblock: &Superblock) -> Vec<u64> {
    let source_id = metadata_at_path(device, superblock, "/src/source")
        .unwrap()
        .inode_id;
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == source_id)
        .unwrap()
        .blocks
}

fn assert_clone(
    device: &mut CrashDevice,
    superblock: &Superblock,
    expected: &[[u8; BLOCK_SIZE]],
    original_source_blocks: &[u64],
) {
    let metadata = metadata_at_path(device, superblock, "/dst/cloned").unwrap();
    assert_eq!(metadata.kind, InodeKind::File);
    assert_eq!(metadata.logical_blocks, expected.len());
    let destination_blocks = load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == metadata.inode_id)
        .unwrap()
        .blocks;
    assert_eq!(destination_blocks.len(), expected.len());
    assert!(destination_blocks
        .iter()
        .all(|block| !original_source_blocks.contains(block)));

    for (index, image) in expected.iter().enumerate() {
        let actual =
            read_file_range_at_path(device, superblock, "/dst/cloned", index, 0, BLOCK_SIZE)
                .unwrap();
        assert_eq!(actual.as_slice(), image.as_slice());
    }
}

#[test]
fn clones_source_block_range_into_fresh_path_with_independent_blocks() {
    let (mut device, superblock, source) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let source_blocks_before = source_blocks(&mut device, &superblock);

    clone_file_blocks_to_path_journaled(
        &mut device,
        &superblock,
        "/src_alias/source",
        1,
        2,
        "/dst_alias/cloned",
    )
    .unwrap();

    let allocator_after = load_allocator(&mut device, &superblock).unwrap();
    assert_eq!(
        allocator_after.allocated_blocks(),
        allocator_before.allocated_blocks() + 2
    );
    assert_eq!(
        source_blocks(&mut device, &superblock),
        source_blocks_before
    );
    assert_clone(
        &mut device,
        &superblock,
        &source[1..],
        &source_blocks_before,
    );
    assert_unique_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
}

#[test]
fn clones_entire_regular_file_into_fresh_path() {
    let (mut device, superblock, source) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let source_blocks_before = source_blocks(&mut device, &superblock);

    clone_file_to_path_journaled(
        &mut device,
        &superblock,
        "/src_alias/source",
        "/dst_alias/cloned",
    )
    .unwrap();

    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        allocator_before.allocated_blocks() + source.len() as u64
    );
    assert_eq!(
        source_blocks(&mut device, &superblock),
        source_blocks_before
    );
    assert_clone(
        &mut device,
        &superblock,
        &source,
        &source_blocks_before,
    );
    assert_unique_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
}

#[test]
fn clones_empty_regular_file_without_allocating_data_blocks() {
    let (mut device, superblock, _) = setup();
    create_empty_file_at_path_journaled(&mut device, &superblock, "/src/empty").unwrap();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inode_count_before = load_inode_table(&mut device, &superblock).unwrap().len();

    clone_file_to_path_journaled(
        &mut device,
        &superblock,
        "/src/empty",
        "/dst_alias/cloned",
    )
    .unwrap();

    let metadata = metadata_at_path(&mut device, &superblock, "/dst/cloned").unwrap();
    assert_eq!(metadata.kind, InodeKind::File);
    assert_eq!(metadata.logical_blocks, 0);
    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap().len(),
        inode_count_before + 1
    );
    assert_unique_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_empty_or_out_of_range_clone_before_destination_creation() {
    let (mut device, superblock, _) = setup();
    for (first, count) in [(0, 0), (3, 1)] {
        assert_eq!(
            clone_file_blocks_to_path_journaled(
                &mut device,
                &superblock,
                "/src/source",
                first,
                count,
                "/dst/cloned",
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            metadata_at_path(&mut device, &superblock, "/dst/cloned")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }
    check_device(&mut device).unwrap();
}

#[test]
fn every_clone_create_crash_point_recovers_absent_or_complete_destination() {
    let (mut probe, superblock, _) = setup();
    probe.arm(None);
    clone_file_blocks_to_path_journaled(
        &mut probe,
        &superblock,
        "/src_alias/source",
        1,
        2,
        "/dst_alias/cloned",
    )
    .unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock, source) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();
        let source_blocks_before = source_blocks(&mut device, &superblock);

        device.arm(Some(crash_at));
        assert_eq!(
            clone_file_blocks_to_path_journaled(
                &mut device,
                &superblock,
                "/src_alias/source",
                1,
                2,
                "/dst_alias/cloned",
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt pathname clone-create"
        );
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        assert_eq!(
            source_blocks(&mut device, &superblock),
            source_blocks_before
        );
        if recovery.committed_transactions == 0 {
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
                entries_before
            );
            assert_eq!(
                metadata_at_path(&mut device, &superblock, "/dst/cloned")
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::NotFound
            );
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(
                load_allocator(&mut device, &superblock)
                    .unwrap()
                    .allocated_blocks(),
                allocator_before.allocated_blocks() + 2
            );
            assert_clone(
                &mut device,
                &superblock,
                &source[1..],
                &source_blocks_before,
            );
        }

        assert_unique_ownership(&mut device, &superblock);
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

#[test]
fn every_whole_file_clone_crash_point_recovers_absent_or_complete_destination() {
    let (mut probe, superblock, _) = setup();
    probe.arm(None);
    clone_file_to_path_journaled(
        &mut probe,
        &superblock,
        "/src_alias/source",
        "/dst_alias/cloned",
    )
    .unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock, source) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();
        let source_blocks_before = source_blocks(&mut device, &superblock);

        device.arm(Some(crash_at));
        assert_eq!(
            clone_file_to_path_journaled(
                &mut device,
                &superblock,
                "/src_alias/source",
                "/dst_alias/cloned",
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt pathname whole-file clone"
        );
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        assert_eq!(
            source_blocks(&mut device, &superblock),
            source_blocks_before
        );
        if recovery.committed_transactions == 0 {
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
                entries_before
            );
            assert_eq!(
                metadata_at_path(&mut device, &superblock, "/dst/cloned")
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::NotFound
            );
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(
                load_allocator(&mut device, &superblock)
                    .unwrap()
                    .allocated_blocks(),
                allocator_before.allocated_blocks() + source.len() as u64
            );
            assert_clone(
                &mut device,
                &superblock,
                &source,
                &source_blocks_before,
            );
        }

        assert_unique_ownership(&mut device, &superblock);
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
