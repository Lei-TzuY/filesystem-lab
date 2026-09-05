mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_data::read_file_block;
use filesystem_lab::file_insert_batch::insert_file_blocks_journaled;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const RANGE_INSERT_JOURNAL_BLOCKS: u64 = 6;

fn setup() -> (CrashDevice, Superblock, u64, u64, Vec<u64>) {
    let mut device = CrashDevice::new(64);
    let superblock =
        format_device_with_journal_blocks(&mut device, RANGE_INSERT_JOURNAL_BLOCKS).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let first = allocator.allocate().unwrap();
    let second = allocator.allocate().unwrap();
    let mut expected = Vec::new();
    for _ in 0..2 {
        let block = allocator.allocate().unwrap();
        expected.push(block);
    }
    for block in &expected {
        allocator.free(*block).unwrap();
    }
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
    (device, superblock, first, second, expected)
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
    expected: &[u64],
) {
    assert_eq!(blocks(device, superblock), vec![first, second]);
    let allocator = load_allocator(device, superblock).unwrap();
    for block in expected {
        assert!(!allocator.is_owned(*block).unwrap());
    }
}

fn assert_new(
    device: &mut CrashDevice,
    superblock: &Superblock,
    first: u64,
    second: u64,
    expected: &[u64],
    data: &[[u8; BLOCK_SIZE]],
) {
    assert_eq!(
        blocks(device, superblock),
        vec![first, expected[0], expected[1], second]
    );
    let allocator = load_allocator(device, superblock).unwrap();
    for block in expected {
        assert!(allocator.is_owned(*block).unwrap());
    }
    assert_eq!(
        read_file_block(device, superblock, 2, 0).unwrap(),
        [0x11; BLOCK_SIZE]
    );
    assert_eq!(read_file_block(device, superblock, 2, 1).unwrap(), data[0]);
    assert_eq!(read_file_block(device, superblock, 2, 2).unwrap(), data[1]);
    assert_eq!(
        read_file_block(device, superblock, 2, 3).unwrap(),
        [0x22; BLOCK_SIZE]
    );
}

#[test]
fn insert_range_allocates_blocks_and_shifts_suffix() {
    let (mut device, superblock, first, second, expected) = setup();
    let data = [[0x5a; BLOCK_SIZE], [0xa5; BLOCK_SIZE]];
    let (allocated, report) =
        insert_file_blocks_journaled(&mut device, &superblock, 2, 1, &data).unwrap();
    assert_eq!(allocated, expected);
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 4);
    assert_new(&mut device, &superblock, first, second, &expected, &data);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn empty_or_out_of_range_insert_is_rejected_before_publication() {
    let (mut device, superblock, first, second, expected) = setup();
    let empty: [[u8; BLOCK_SIZE]; 0] = [];
    assert_eq!(
        insert_file_blocks_journaled(&mut device, &superblock, 2, 1, &empty)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        insert_file_blocks_journaled(&mut device, &superblock, 2, 3, &[[0x33; BLOCK_SIZE]])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_old(&mut device, &superblock, first, second, &expected);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_insert_range_mutation_crash_point_is_old_or_recoverable_new_state() {
    let data = [[0xa7; BLOCK_SIZE], [0x7a; BLOCK_SIZE]];
    let (mut probe, superblock, first, second, expected) = setup();
    probe.arm(None);
    let (_, report) = insert_file_blocks_journaled(&mut probe, &superblock, 2, 1, &data).unwrap();
    assert_eq!(report.home_writes, 4);
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 8);
    assert_new(&mut probe, &superblock, first, second, &expected, &data);

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, first, second, expected) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            insert_file_blocks_journaled(&mut device, &superblock, 2, 1, &data)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();

        let allocator = load_allocator(&mut device, &superblock).unwrap();
        let owned: Vec<_> = expected
            .iter()
            .map(|block| allocator.is_owned(*block).unwrap())
            .collect();
        let raw_blocks = blocks(&mut device, &superblock);
        let raw_is_old = owned == vec![false, false] && raw_blocks == vec![first, second];
        let raw_metadata_is_new = owned == vec![true, true]
            && raw_blocks == vec![first, expected[0], expected[1], second];
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
            assert_old(&mut device, &superblock, first, second, &expected);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(recovery.home_writes, 4);
            assert_new(&mut device, &superblock, first, second, &expected, &data);
        }
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());

        let second_recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second_recovery, RecoveryReport::default());
    }
}
