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
use filesystem_lab::path_zero_range::zero_file_at_path_journaled;
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

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[inode(1, InodeKind::Directory), inode(2, InodeKind::File)],
    )
    .unwrap();
    store_directory_table(&mut device, &superblock, &[entry(1, 2, "file")]).unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "alias", "/file").unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/file",
        &[[0x31; BLOCK_SIZE], [0x42; BLOCK_SIZE]],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn zeroes_complete_file_through_final_symlink_without_metadata_changes() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let directory_before = load_directory_table(&mut device, &superblock).unwrap();

    zero_file_at_path_journaled(&mut device, &superblock, "/alias").unwrap();

    assert_eq!(
        read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap(),
        vec![[0_u8; BLOCK_SIZE], [0_u8; BLOCK_SIZE]]
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
    check_device(&mut device).unwrap();
}

#[test]
fn every_whole_file_zero_crash_point_recovers_old_or_complete_zero_image() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    zero_file_at_path_journaled(&mut probe, &superblock, "/file").unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let directory_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert_eq!(
            zero_file_at_path_journaled(&mut device, &superblock, "/file")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let blocks = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
        let old = vec![[0x31; BLOCK_SIZE], [0x42; BLOCK_SIZE]];
        let zero = vec![[0_u8; BLOCK_SIZE], [0_u8; BLOCK_SIZE]];
        assert!(blocks == old || blocks == zero);
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
        check_device(&mut device).unwrap();
        assert!(
            load_journal_image(&mut device, superblock)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }
}
