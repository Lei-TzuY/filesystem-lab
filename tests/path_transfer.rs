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
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::path_transfer::{
    transfer_file_block_range_at_path_journaled, PathFileBlockTransfer,
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

fn endpoint(path: &str, index: usize) -> PathFileBlockTransfer<'_> {
    PathFileBlockTransfer { path, index }
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
            entry(2, 3, "source"),
            entry(2, 4, "destination"),
        ],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "source_alias", "/dir/source").unwrap();
    create_symlink_journaled(
        &mut device,
        &superblock,
        1,
        "destination_alias",
        "/dir/destination",
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dangling", "/missing").unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/dir/source",
        &[[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE], [0x33; BLOCK_SIZE]],
    )
    .unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/dir/destination",
        &[[0x44; BLOCK_SIZE], [0x55; BLOCK_SIZE]],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn transfer_middle(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> io::Result<RecoveryReport> {
    transfer_file_block_range_at_path_journaled(
        device,
        superblock,
        endpoint("/source_alias", 1),
        1,
        endpoint("/destination_alias", 1),
    )
    .map(|(_, report)| report)
}

#[test]
fn transfers_block_range_through_direct_and_symlink_paths() {
    let (mut device, superblock) = setup();
    let (moved, _) = transfer_file_block_range_at_path_journaled(
        &mut device,
        &superblock,
        endpoint("/dir_alias/source", 1),
        1,
        endpoint("/destination_alias", 1),
    )
    .unwrap();
    assert_eq!(moved.len(), 1);
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/source", 1, 0, 1).unwrap(),
        vec![0x33]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/destination", 1, 0, 1).unwrap(),
        vec![0x22]
    );
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_invalid_path_transfer_without_publication() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let directory_before = load_directory_table(&mut device, &superblock).unwrap();
    for result in [
        transfer_file_block_range_at_path_journaled(
            &mut device,
            &superblock,
            endpoint("/dir", 0),
            1,
            endpoint("/dir/destination", 0),
        ),
        transfer_file_block_range_at_path_journaled(
            &mut device,
            &superblock,
            endpoint("/dangling", 0),
            1,
            endpoint("/dir/destination", 0),
        ),
        transfer_file_block_range_at_path_journaled(
            &mut device,
            &superblock,
            endpoint("/dir/source", 0),
            1,
            endpoint("/source_alias", 0),
        ),
        transfer_file_block_range_at_path_journaled(
            &mut device,
            &superblock,
            endpoint("/dir/source", 0),
            0,
            endpoint("/dir/destination", 0),
        ),
        transfer_file_block_range_at_path_journaled(
            &mut device,
            &superblock,
            endpoint("/dir/source", 3),
            1,
            endpoint("/dir/destination", 0),
        ),
    ] {
        assert!(matches!(
            result.unwrap_err().kind(),
            io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
        ));
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
fn every_pathname_transfer_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    let allocator_before = load_allocator(&mut probe, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut probe, &superblock).unwrap();
    let directory_before = load_directory_table(&mut probe, &superblock).unwrap();
    probe.arm(None);
    transfer_middle(&mut probe, &superblock).unwrap();
    let operations = probe.operations();
    let inodes_after = load_inode_table(&mut probe, &superblock).unwrap();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            transfer_middle(&mut device, &superblock)
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
