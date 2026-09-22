mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_file_read::read_file_blocks_at_path;
use filesystem_lab::path_grow::{
    grow_file_at_path_to_bytes_journaled, resize_file_at_path_to_bytes_journaled,
};
use filesystem_lab::path_metadata::metadata_at_path;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn install_namespace(
    device: &mut CrashDevice,
    superblock: &Superblock,
    file: PersistedInode,
) {
    let root = PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap();
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
}

fn empty_file() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    install_namespace(
        &mut device,
        &superblock,
        PersistedInode::new(2, InodeKind::File, Vec::new()).unwrap(),
    );
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn partial_file() -> (CrashDevice, Superblock, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let first = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();

    let mut image = [0_u8; BLOCK_SIZE];
    image[..3000].fill(0xa5);
    device.write_block(first, &image).unwrap();
    device.flush().unwrap();

    install_namespace(
        &mut device,
        &superblock,
        PersistedInode::new_file_with_size(2, vec![first], 3000).unwrap(),
    );
    check_device(&mut device).unwrap();
    (device, superblock, first)
}

fn read_block(device: &mut CrashDevice, block: u64) -> [u8; BLOCK_SIZE] {
    let mut image = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut image).unwrap();
    image
}

#[test]
fn grows_empty_file_to_exact_partial_eof_with_only_required_blocks() {
    let (mut device, superblock) = empty_file();

    let (allocated, report) =
        grow_file_at_path_to_bytes_journaled(&mut device, &superblock, "/file", 5000).unwrap();

    assert_eq!(allocated.len(), 2);
    assert_eq!(report.committed_transactions, 1);
    let metadata = metadata_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(metadata.logical_blocks, 2);
    assert_eq!(metadata.byte_len, 5000);
    let data = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(data.len(), 5000);
    assert!(data.iter().all(|byte| *byte == 0));
    check_device(&mut device).unwrap();
}

#[test]
fn same_block_growth_changes_only_eof_and_keeps_newly_visible_bytes_zero() {
    let (mut device, superblock, first) = partial_file();
    let allocated_before = load_allocator(&mut device, &superblock)
        .unwrap()
        .allocated_blocks();

    let (allocated, report) =
        grow_file_at_path_to_bytes_journaled(&mut device, &superblock, "/file", 3500).unwrap();

    assert!(allocated.is_empty());
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        allocated_before
    );
    let metadata = metadata_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(metadata.logical_blocks, 1);
    assert_eq!(metadata.byte_len, 3500);
    let image = read_block(&mut device, first);
    assert!(image[..3000].iter().all(|byte| *byte == 0xa5));
    assert!(image[3000..].iter().all(|byte| *byte == 0));
    let data = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(data.len(), 3500);
    assert!(data[..3000].iter().all(|byte| *byte == 0xa5));
    assert!(data[3000..].iter().all(|byte| *byte == 0));
    check_device(&mut device).unwrap();
}

#[test]
fn byte_resize_composes_exact_shrink_and_zero_growth() {
    let (mut device, superblock) = empty_file();

    resize_file_at_path_to_bytes_journaled(&mut device, &superblock, "/file", 5000).unwrap();
    resize_file_at_path_to_bytes_journaled(&mut device, &superblock, "/file", 2500).unwrap();
    let (allocated, _) =
        resize_file_at_path_to_bytes_journaled(&mut device, &superblock, "/file", 6000).unwrap();

    assert_eq!(allocated.len(), 1);
    let metadata = metadata_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(metadata.logical_blocks, 2);
    assert_eq!(metadata.byte_len, 6000);
    let data = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(data.len(), 6000);
    assert!(data.iter().all(|byte| *byte == 0));
    assert_eq!(
        resize_file_at_path_to_bytes_journaled(&mut device, &superblock, "/file", 6000)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    check_device(&mut device).unwrap();
}

#[test]
fn byte_growth_crash_matrix_recovers_old_or_complete_new_state() {
    let (prepared, superblock, first) = partial_file();

    let mut probe = prepared.clone();
    probe.arm(None);
    grow_file_at_path_to_bytes_journaled(&mut probe, &superblock, "/file", 5000).unwrap();
    let mutation_count = probe.operations();
    assert!(mutation_count > 0);

    for crash_at in 0..mutation_count {
        let mut device = prepared.clone();
        device.arm(Some(crash_at));
        assert!(
            grow_file_at_path_to_bytes_journaled(&mut device, &superblock, "/file", 5000)
                .is_err(),
            "crash_at={crash_at}"
        );

        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let inodes = load_inode_table(&mut device, &superblock).unwrap();
        let file = inodes.iter().find(|inode| inode.id == 2).unwrap();
        let allocator = load_allocator(&mut device, &superblock).unwrap();
        match file.byte_len {
            3000 => {
                assert_eq!(file.blocks, vec![first], "crash_at={crash_at}");
                assert_eq!(allocator.allocated_blocks(), 1, "crash_at={crash_at}");
                let image = read_block(&mut device, first);
                assert!(image[..3000].iter().all(|byte| *byte == 0xa5));
                assert!(image[3000..].iter().all(|byte| *byte == 0));
            }
            5000 => {
                assert_eq!(file.blocks.len(), 2, "crash_at={crash_at}");
                assert_eq!(file.blocks[0], first, "crash_at={crash_at}");
                assert_eq!(allocator.allocated_blocks(), 2, "crash_at={crash_at}");
                let first_image = read_block(&mut device, first);
                assert!(first_image[..3000].iter().all(|byte| *byte == 0xa5));
                assert!(first_image[3000..].iter().all(|byte| *byte == 0));
                assert_eq!(read_block(&mut device, file.blocks[1]), [0_u8; BLOCK_SIZE]);
            }
            other => panic!("crash_at={crash_at}: unexpected EOF {other}"),
        }

        assert!(
            load_journal_image(&mut device, superblock)
                .unwrap()
                .is_empty(),
            "crash_at={crash_at}"
        );
        check_device(&mut device).unwrap();
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default(),
            "crash_at={crash_at}"
        );
    }
}
