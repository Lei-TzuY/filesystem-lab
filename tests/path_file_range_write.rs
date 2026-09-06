mod support;

use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::file_data::append_file_block_journaled;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_lookup::{read_file_range_at_path, write_file_range_at_path_journaled};
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 6;

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
    let mut device = CrashDevice::new(96);
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
    append_file_block_journaled(&mut device, &superblock, 3, [0x11; BLOCK_SIZE]).unwrap();
    append_file_block_journaled(&mut device, &superblock, 3, [0x22; BLOCK_SIZE]).unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/dir/file").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dangling", "/missing").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn write_crossing(device: &mut CrashDevice, superblock: &Superblock) -> io::Result<RecoveryReport> {
    write_file_range_at_path_journaled(
        device,
        superblock,
        "/file_alias",
        0,
        BLOCK_SIZE - 8,
        &[0xaa; 16],
    )
}

#[test]
fn writes_regular_file_ranges_through_direct_and_symlink_paths() {
    let (mut device, superblock) = setup();

    write_file_range_at_path_journaled(
        &mut device,
        &superblock,
        "/dir/file",
        0,
        100,
        b"direct",
    )
    .unwrap();
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 0, 100, 6).unwrap(),
        b"direct"
    );

    write_file_range_at_path_journaled(
        &mut device,
        &superblock,
        "/dir_alias/file",
        0,
        200,
        b"middle",
    )
    .unwrap();
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/file_alias", 0, 200, 6).unwrap(),
        b"middle"
    );
    check_device(&mut device).unwrap();
}

#[test]
fn propagates_path_and_existing_range_write_validation_before_publication() {
    let (mut device, superblock) = setup();
    let before = read_file_range_at_path(&mut device, &superblock, "/dir/file", 0, 0, 8).unwrap();

    assert_eq!(
        write_file_range_at_path_journaled(&mut device, &superblock, "/dir", 0, 0, b"x")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        write_file_range_at_path_journaled(&mut device, &superblock, "/dangling", 0, 0, b"x")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        write_file_range_at_path_journaled(
            &mut device,
            &superblock,
            "/dir/file",
            1,
            BLOCK_SIZE - 1,
            b"xx",
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 0, 0, 8).unwrap(),
        before
    );
    assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
}

#[test]
fn every_pathname_write_crash_point_recovers_old_or_complete_new_data() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    write_crossing(&mut probe, &superblock).unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let directory_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert_eq!(
            write_crossing(&mut device, &superblock).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        check_device(&mut device).unwrap();

        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        let first = read_file_range_at_path(
            &mut device,
            &superblock,
            "/dir/file",
            0,
            BLOCK_SIZE - 8,
            8,
        )
        .unwrap();
        let second =
            read_file_range_at_path(&mut device, &superblock, "/dir/file", 1, 0, 8).unwrap();
        if report.committed_transactions == 0 {
            assert_eq!(first, vec![0x11; 8]);
            assert_eq!(second, vec![0x22; 8]);
        } else {
            assert_eq!(first, vec![0xaa; 8]);
            assert_eq!(second, vec![0xaa; 8]);
        }

        assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes_before);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            directory_before
        );
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }
}
