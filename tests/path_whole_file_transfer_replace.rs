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
use filesystem_lab::path_file_read::read_file_blocks_at_path;
use filesystem_lab::path_whole_transfer_replace::transfer_replace_complete_file_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 12;

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
            inode(3, InodeKind::File),
            inode(4, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 2, "source"),
            entry(1, 3, "destination"),
            entry(1, 4, "empty"),
        ],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "source_alias", "/source").unwrap();
    create_symlink_journaled(
        &mut device,
        &superblock,
        1,
        "destination_alias",
        "/destination",
    )
    .unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/source",
        &[[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE]],
    )
    .unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/destination",
        &[[0x33; BLOCK_SIZE]],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn transfer_source_over_destination(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> io::Result<RecoveryReport> {
    transfer_replace_complete_file_at_path_journaled(
        device,
        superblock,
        "/source_alias",
        "/destination_alias",
    )
}

#[test]
fn transfers_complete_source_over_destination_and_releases_displaced_blocks() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let source_before = read_file_blocks_at_path(&mut device, &superblock, "/source").unwrap();

    transfer_source_over_destination(&mut device, &superblock).unwrap();

    assert!(
        read_file_blocks_at_path(&mut device, &superblock, "/source")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        read_file_blocks_at_path(&mut device, &superblock, "/destination").unwrap(),
        source_before
    );
    let allocator_after = load_allocator(&mut device, &superblock).unwrap();
    assert_eq!(
        allocator_after.allocated_blocks() + 1,
        allocator_before.allocated_blocks()
    );
    check_device(&mut device).unwrap();
}

#[test]
fn empty_source_atomically_clears_destination_and_releases_its_blocks() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();

    transfer_replace_complete_file_at_path_journaled(
        &mut device,
        &superblock,
        "/empty",
        "/destination",
    )
    .unwrap();

    assert!(read_file_blocks_at_path(&mut device, &superblock, "/empty")
        .unwrap()
        .is_empty());
    assert!(
        read_file_blocks_at_path(&mut device, &superblock, "/destination")
            .unwrap()
            .is_empty()
    );
    let allocator_after = load_allocator(&mut device, &superblock).unwrap();
    assert_eq!(
        allocator_after.allocated_blocks() + 1,
        allocator_before.allocated_blocks()
    );
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_same_inode_or_non_file_without_publication() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let directory_before = load_directory_table(&mut device, &superblock).unwrap();

    assert_eq!(
        transfer_replace_complete_file_at_path_journaled(
            &mut device,
            &superblock,
            "/source",
            "/source_alias",
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        transfer_replace_complete_file_at_path_journaled(
            &mut device,
            &superblock,
            "/",
            "/destination",
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
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        directory_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_whole_file_transfer_replace_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    let allocator_before = load_allocator(&mut probe, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut probe, &superblock).unwrap();
    let directory_before = load_directory_table(&mut probe, &superblock).unwrap();
    probe.arm(None);
    transfer_source_over_destination(&mut probe, &superblock).unwrap();
    let operations = probe.operations();
    let allocator_after = load_allocator(&mut probe, &superblock).unwrap();
    let inodes_after = load_inode_table(&mut probe, &superblock).unwrap();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            transfer_source_over_destination(&mut device, &superblock)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let recovered_allocator = load_allocator(&mut device, &superblock).unwrap();
        let recovered_inodes = load_inode_table(&mut device, &superblock).unwrap();
        assert!(
            (recovered_allocator == allocator_before && recovered_inodes == inodes_before)
                || (recovered_allocator == allocator_after && recovered_inodes == inodes_after)
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
