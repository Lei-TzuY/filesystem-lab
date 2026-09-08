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
use filesystem_lab::path_overwrite_batch::write_file_blocks_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 10;
const NEW_FIRST: [u8; BLOCK_SIZE] = [0xa1; BLOCK_SIZE];
const NEW_THIRD: [u8; BLOCK_SIZE] = [0xc3; BLOCK_SIZE];

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
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/dir/file",
        &[
            [0x11; BLOCK_SIZE],
            [0x22; BLOCK_SIZE],
            [0x33; BLOCK_SIZE],
        ],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn overwrite_noncontiguous(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> io::Result<RecoveryReport> {
    write_file_blocks_at_path_journaled(
        device,
        superblock,
        "/file_alias",
        &[(0, NEW_FIRST), (2, NEW_THIRD)],
    )
}

#[test]
fn overwrites_noncontiguous_blocks_through_direct_and_symlink_paths() {
    let (mut device, superblock) = setup();
    let report = overwrite_noncontiguous(&mut device, &superblock).unwrap();
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 2);
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir_alias/file", 0, 0, 1).unwrap(),
        vec![0xa1]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 1, 0, 1).unwrap(),
        vec![0x22]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 2, 0, 1).unwrap(),
        vec![0xc3]
    );
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_invalid_batches_without_publication() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let directory_before = load_directory_table(&mut device, &superblock).unwrap();

    for result in [
        write_file_blocks_at_path_journaled(&mut device, &superblock, "/dir/file", &[]),
        write_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/dir/file",
            &[(0, NEW_FIRST), (0, NEW_THIRD)],
        ),
        write_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/dir/file",
            &[(3, NEW_FIRST)],
        ),
        write_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/dir",
            &[(0, NEW_FIRST)],
        ),
    ] {
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }
    assert_eq!(
        write_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/dangling",
            &[(0, NEW_FIRST)],
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::NotFound
    );

    assert_eq!(load_allocator(&mut device, &superblock).unwrap(), allocator_before);
    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes_before);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        directory_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_pathname_batch_overwrite_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    overwrite_noncontiguous(&mut probe, &superblock).unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let directory_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert_eq!(
            overwrite_noncontiguous(&mut device, &superblock)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let first = read_file_range_at_path(&mut device, &superblock, "/dir/file", 0, 0, 1)
            .unwrap()[0];
        let middle = read_file_range_at_path(&mut device, &superblock, "/dir/file", 1, 0, 1)
            .unwrap()[0];
        let third = read_file_range_at_path(&mut device, &superblock, "/dir/file", 2, 0, 1)
            .unwrap()[0];
        assert!((first, middle, third) == (0x11, 0x22, 0x33)
            || (first, middle, third) == (0xa1, 0x22, 0xc3));
        assert_eq!(load_allocator(&mut device, &superblock).unwrap(), allocator_before);
        assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes_before);
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
