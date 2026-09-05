mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_data::read_file_block;
use filesystem_lab::file_insert::insert_file_block_journaled;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn setup() -> (CrashDevice, Superblock, u64, u64, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let first = allocator.allocate().unwrap();
    let second = allocator.allocate().unwrap();
    let inserted = allocator.allocate().unwrap();
    allocator.free(inserted).unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            PersistedInode {
                id: 1,
                kind: InodeKind::Directory,
                blocks: Vec::new(),
            },
            PersistedInode {
                id: 2,
                kind: InodeKind::File,
                blocks: vec![first, second],
            },
        ],
    )
    .unwrap();
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
    device.write_block(first, &[0x11; BLOCK_SIZE]).unwrap();
    device.write_block(second, &[0x22; BLOCK_SIZE]).unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, first, second, inserted)
}

fn blocks(device: &mut CrashDevice, superblock: &Superblock) -> Vec<u64> {
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap()
        .blocks
}

fn assert_old(
    device: &mut CrashDevice,
    superblock: &Superblock,
    first: u64,
    second: u64,
    inserted: u64,
) {
    assert_eq!(blocks(device, superblock), vec![first, second]);
    assert!(
        !load_allocator(device, superblock)
            .unwrap()
            .is_owned(inserted)
            .unwrap()
    );
}

fn assert_new(
    device: &mut CrashDevice,
    superblock: &Superblock,
    first: u64,
    second: u64,
    inserted: u64,
    data: &[u8; BLOCK_SIZE],
) {
    assert_eq!(
        blocks(device, superblock),
        vec![first, inserted, second]
    );
    assert!(
        load_allocator(device, superblock)
            .unwrap()
            .is_owned(inserted)
            .unwrap()
    );
    assert_eq!(
        read_file_block(device, superblock, 2, 0).unwrap(),
        [0x11; BLOCK_SIZE]
    );
    assert_eq!(
        read_file_block(device, superblock, 2, 1).unwrap(),
        *data
    );
    assert_eq!(
        read_file_block(device, superblock, 2, 2).unwrap(),
        [0x22; BLOCK_SIZE]
    );
}

#[test]
fn insert_allocates_one_block_and_shifts_logical_suffix() {
    let (mut device, superblock, first, second, expected) = setup();
    let data = [0x5a; BLOCK_SIZE];
    let (block, report) =
        insert_file_block_journaled(&mut device, &superblock, 2, 1, data).unwrap();
    assert_eq!(block, expected);
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 3);
    assert_new(
        &mut device,
        &superblock,
        first,
        second,
        expected,
        &data,
    );
    assert!(
        load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty()
    );
    check_device(&mut device).unwrap();
}

#[test]
fn insertion_beyond_end_is_rejected_before_publication() {
    let (mut device, superblock, first, second, expected) = setup();
    let error =
        insert_file_block_journaled(&mut device, &superblock, 2, 3, [0x33; BLOCK_SIZE])
            .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_old(&mut device, &superblock, first, second, expected);
    assert!(
        load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn every_insert_mutation_crash_point_is_old_or_recoverable_new_state() {
    let data = [0xa7; BLOCK_SIZE];
    let (mut probe, superblock, first, second, expected) = setup();
    probe.arm(None);
    let (_, report) =
        insert_file_block_journaled(&mut probe, &superblock, 2, 1, data).unwrap();
    assert_eq!(report.home_writes, 3);
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 7);
    assert_new(
        &mut probe,
        &superblock,
        first,
        second,
        expected,
        &data,
    );

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, first, second, expected) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            insert_file_block_journaled(&mut device, &superblock, 2, 1, data)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();

        let raw_owned = load_allocator(&mut device, &superblock)
            .unwrap()
            .is_owned(expected)
            .unwrap();
        let raw_blocks = blocks(&mut device, &superblock);
        let raw_is_old = !raw_owned && raw_blocks == vec![first, second];
        let raw_metadata_is_new = raw_owned && raw_blocks == vec![first, expected, second];
        if raw_is_old || raw_metadata_is_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(
                check_device(&mut device).is_err(),
                "crash point {crash_at} exposed mixed metadata accepted by fsck"
            );
        }

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_old(&mut device, &superblock, first, second, expected);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(recovery.home_writes, 3);
            assert_new(
                &mut device,
                &superblock,
                first,
                second,
                expected,
                &data,
            );
        }
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
