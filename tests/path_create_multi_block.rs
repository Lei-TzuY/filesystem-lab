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
use filesystem_lab::path_create::create_file_with_blocks_at_path_journaled;
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

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(160);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
        ],
    )
    .unwrap();
    store_directory_table(&mut device, &superblock, &[entry(1, 2, "dir")]).unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
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

fn assert_created(
    device: &mut CrashDevice,
    superblock: &Superblock,
    path: &str,
    expected: &[[u8; BLOCK_SIZE]],
) {
    let metadata = metadata_at_path(device, superblock, path).unwrap();
    assert_eq!(metadata.kind, InodeKind::File);
    assert_eq!(metadata.logical_blocks, expected.len());
    assert_eq!(metadata.namespace_references, 1);

    let inodes = load_inode_table(device, superblock).unwrap();
    let inode = inodes
        .iter()
        .find(|inode| inode.id == metadata.inode_id)
        .unwrap();
    assert_eq!(inode.blocks.len(), expected.len());

    let allocator = load_allocator(device, superblock).unwrap();
    for block in &inode.blocks {
        assert!(allocator.is_owned(*block).unwrap());
    }

    for (index, image) in expected.iter().enumerate() {
        let actual =
            read_file_range_at_path(device, superblock, path, index, 0, BLOCK_SIZE).unwrap();
        assert_eq!(actual.as_slice(), image.as_slice());
    }
}

#[test]
fn creates_initialized_multi_block_file_through_symlinked_parent() {
    let (mut device, superblock) = setup();
    let data = [
        [0x11_u8; BLOCK_SIZE],
        [0x22_u8; BLOCK_SIZE],
        [0x33_u8; BLOCK_SIZE],
    ];
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    create_file_with_blocks_at_path_journaled(&mut device, &superblock, "/dir_alias/seeded", &data)
        .unwrap();

    let allocator_after = load_allocator(&mut device, &superblock).unwrap();
    assert_eq!(
        allocator_after.allocated_blocks(),
        allocator_before.allocated_blocks() + u64::try_from(data.len()).unwrap()
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap().len(),
        inodes_before.len() + 1
    );
    assert_created(&mut device, &superblock, "/dir/seeded", &data);
    assert_unique_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn rejects_empty_multi_block_create_without_namespace_or_allocator_change() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();

    assert_eq!(
        create_file_with_blocks_at_path_journaled(&mut device, &superblock, "/dir/empty", &[])
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
        entries_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn every_multi_block_create_crash_point_recovers_old_or_complete_new_file() {
    let data = [
        [0x41_u8; BLOCK_SIZE],
        [0x52_u8; BLOCK_SIZE],
        [0x63_u8; BLOCK_SIZE],
    ];
    let (mut probe, superblock) = setup();
    probe.arm(None);
    create_file_with_blocks_at_path_journaled(&mut probe, &superblock, "/dir_alias/seeded", &data)
        .unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert_eq!(
            create_file_with_blocks_at_path_journaled(
                &mut device,
                &superblock,
                "/dir_alias/seeded",
                &data,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt pathname multi-block creation"
        );
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

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
                metadata_at_path(&mut device, &superblock, "/dir/seeded")
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
                allocator_before.allocated_blocks() + u64::try_from(data.len()).unwrap()
            );
            assert_eq!(
                load_inode_table(&mut device, &superblock).unwrap().len(),
                inodes_before.len() + 1
            );
            assert_eq!(
                load_directory_table(&mut device, &superblock)
                    .unwrap()
                    .len(),
                entries_before.len() + 1
            );
            assert_created(&mut device, &superblock, "/dir/seeded", &data);
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
