mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_file_read::read_file_blocks_at_path;
use filesystem_lab::path_file_write::write_file_range_extending_at_path_journaled;
use filesystem_lab::path_metadata::metadata_at_path;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 12;

fn install_partial_file(device: &mut CrashDevice, superblock: &Superblock) -> u64 {
    let mut allocator = load_allocator(device, superblock).unwrap();
    let first = allocator.allocate().unwrap();
    store_allocator(device, superblock, &allocator).unwrap();

    let mut image = [0_u8; BLOCK_SIZE];
    image[..3000].fill(0xa5);
    device.write_block(first, &image).unwrap();
    device.flush().unwrap();

    let root = PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap();
    let file = PersistedInode::new_file_with_size(2, vec![first], 3000).unwrap();
    store_inode_table(device, superblock, &[root, file]).unwrap();
    store_directory_table(
        device,
        superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "file".to_owned(),
        }],
    )
    .unwrap();
    check_device(device).unwrap();
    first
}

fn partial_file() -> (CrashDevice, Superblock, u64) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    let first = install_partial_file(&mut device, &superblock);
    (device, superblock, first)
}

fn empty_file_default_journal() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device(&mut device).unwrap();
    let root = PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap();
    let file = PersistedInode::new(2, InodeKind::File, Vec::new()).unwrap();
    store_inode_table(&mut device, &superblock, &[root, file]).unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "file".to_owned(),
        }],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn read_block(device: &mut CrashDevice, block: u64) -> [u8; BLOCK_SIZE] {
    let mut image = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut image).unwrap();
    image
}

#[test]
fn same_block_extending_write_zero_fills_gap_and_advances_eof() {
    let (mut device, superblock, first) = partial_file();

    let (allocated, report) = write_file_range_extending_at_path_journaled(
        &mut device,
        &superblock,
        "/file",
        3200,
        b"payload",
    )
    .unwrap();

    assert!(allocated.is_empty());
    assert_eq!(report.committed_transactions, 1);
    let metadata = metadata_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(metadata.logical_blocks, 1);
    assert_eq!(metadata.byte_len, 3207);

    let bytes = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(bytes.len(), 3207);
    assert!(bytes[..3000].iter().all(|byte| *byte == 0xa5));
    assert!(bytes[3000..3200].iter().all(|byte| *byte == 0));
    assert_eq!(&bytes[3200..], b"payload");

    let image = read_block(&mut device, first);
    assert!(image[3207..].iter().all(|byte| *byte == 0));
    check_device(&mut device).unwrap();
}

#[test]
fn cross_block_extending_write_allocates_only_required_suffix() {
    let (mut device, superblock, first) = partial_file();
    let payload = vec![0xcc; 100];

    let (allocated, report) = write_file_range_extending_at_path_journaled(
        &mut device,
        &superblock,
        "/file",
        5000,
        &payload,
    )
    .unwrap();

    assert_eq!(allocated.len(), 1);
    assert_eq!(report.committed_transactions, 1);
    let metadata = metadata_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(metadata.logical_blocks, 2);
    assert_eq!(metadata.byte_len, 5100);

    let bytes = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(bytes.len(), 5100);
    assert!(bytes[..3000].iter().all(|byte| *byte == 0xa5));
    assert!(bytes[3000..5000].iter().all(|byte| *byte == 0));
    assert_eq!(&bytes[5000..], payload.as_slice());

    assert_eq!(load_allocator(&mut device, &superblock).unwrap().allocated_blocks(), 2);
    assert_eq!(load_inode_table(&mut device, &superblock).unwrap()[1].blocks[0], first);
    let second = load_inode_table(&mut device, &superblock).unwrap()[1].blocks[1];
    let second_image = read_block(&mut device, second);
    assert!(second_image[1004..].iter().all(|byte| *byte == 0));
    check_device(&mut device).unwrap();
}

#[test]
fn overwrite_inside_eof_keeps_size_and_allocates_nothing() {
    let (mut device, superblock, _) = partial_file();

    let (allocated, report) = write_file_range_extending_at_path_journaled(
        &mut device,
        &superblock,
        "/file",
        100,
        b"inside",
    )
    .unwrap();

    assert!(allocated.is_empty());
    assert_eq!(report.committed_transactions, 1);
    let metadata = metadata_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(metadata.byte_len, 3000);
    let bytes = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(&bytes[100..106], b"inside");
    check_device(&mut device).unwrap();
}

#[test]
fn insufficient_journal_capacity_fails_before_home_mutation() {
    let (mut device, superblock) = empty_file_default_journal();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    let error = write_file_range_extending_at_path_journaled(
        &mut device,
        &superblock,
        "/file",
        0,
        &vec![0x77; 5000],
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error
        .to_string()
        .contains("journal image exceeds reserved region"));
    assert_eq!(load_allocator(&mut device, &superblock).unwrap(), allocator_before);
    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes_before);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn extending_write_crash_matrix_recovers_old_or_complete_new_file() {
    let payload = vec![0xcc; 100];
    let (prepared, superblock, first) = partial_file();

    let mut probe = prepared.clone();
    probe.arm(None);
    write_file_range_extending_at_path_journaled(
        &mut probe,
        &superblock,
        "/file",
        5000,
        &payload,
    )
    .unwrap();
    let mutation_count = probe.operations();
    assert!(mutation_count > 0);

    for crash_at in 0..mutation_count {
        let (mut device, superblock, _) = partial_file();
        device.arm(Some(crash_at));
        assert!(
            write_file_range_extending_at_path_journaled(
                &mut device,
                &superblock,
                "/file",
                5000,
                &payload,
            )
            .is_err(),
            "crash_at={crash_at}"
        );

        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        let inodes = load_inode_table(&mut device, &superblock).unwrap();
        let file = inodes.iter().find(|inode| inode.id == 2).unwrap();
        let allocator = load_allocator(&mut device, &superblock).unwrap();

        match file.byte_len {
            3000 => {
                assert_eq!(file.blocks, vec![first], "crash_at={crash_at}");
                assert_eq!(allocator.allocated_blocks(), 1, "crash_at={crash_at}");
                let bytes = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
                assert_eq!(bytes.len(), 3000);
                assert!(bytes.iter().all(|byte| *byte == 0xa5));
            }
            5100 => {
                assert_eq!(file.blocks.len(), 2, "crash_at={crash_at}");
                assert_eq!(file.blocks[0], first, "crash_at={crash_at}");
                assert_eq!(allocator.allocated_blocks(), 2, "crash_at={crash_at}");
                let bytes = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
                assert_eq!(bytes.len(), 5100);
                assert!(bytes[..3000].iter().all(|byte| *byte == 0xa5));
                assert!(bytes[3000..5000].iter().all(|byte| *byte == 0));
                assert_eq!(&bytes[5000..], payload.as_slice());
            }
            other => panic!("crash_at={crash_at}: unexpected EOF {other}"),
        }

        assert!(
            recovery == RecoveryReport::default()
                || recovery.committed_transactions == 1,
            "crash_at={crash_at}, recovery={recovery:?}"
        );
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
        check_device(&mut device).unwrap();
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default(),
            "crash_at={crash_at}"
        );
    }
}
