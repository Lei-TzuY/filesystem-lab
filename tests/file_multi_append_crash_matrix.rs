mod support;

use std::io;

use filesystem_lab::allocation_disk::{initialize_allocation_region, load_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{initialize_directory_table_region, store_directory_table};
use filesystem_lab::file_append_batch::append_file_blocks_journaled;
use filesystem_lab::file_data::read_file_block;
use filesystem_lab::format::{Superblock, SUPERBLOCK_BLOCK};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{
    initialize_inode_table_region, load_inode_table, store_inode_table,
};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const MULTI_APPEND_JOURNAL_BLOCKS: u64 = 6;

fn setup() -> (CrashDevice, Superblock, Vec<u64>) {
    let mut device = CrashDevice::new(64);
    let superblock =
        Superblock::with_journal_blocks(device.block_count(), MULTI_APPEND_JOURNAL_BLOCKS).unwrap();
    initialize_allocation_region(&mut device, &superblock).unwrap();
    initialize_inode_table_region(&mut device, &superblock).unwrap();
    initialize_directory_table_region(&mut device, &superblock).unwrap();
    device
        .write_block(SUPERBLOCK_BLOCK, &superblock.encode())
        .unwrap();
    device.flush().unwrap();

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
                blocks: Vec::new(),
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
    device.flush().unwrap();
    check_device(&mut device).unwrap();

    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let first = allocator.allocate().unwrap();
    let second = allocator.allocate().unwrap();
    (device, superblock, vec![first, second])
}

fn assert_old(device: &mut CrashDevice, superblock: &Superblock, expected: &[u64]) {
    let allocator = load_allocator(device, superblock).unwrap();
    for block in expected {
        assert!(!allocator.is_owned(*block).unwrap());
    }
    let inode = load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap();
    assert!(inode.blocks.is_empty());
}

fn assert_new(
    device: &mut CrashDevice,
    superblock: &Superblock,
    expected: &[u64],
    data: &[[u8; BLOCK_SIZE]],
) {
    let allocator = load_allocator(device, superblock).unwrap();
    for block in expected {
        assert!(allocator.is_owned(*block).unwrap());
    }
    let inode = load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap();
    assert_eq!(inode.blocks, expected);
    for (index, image) in data.iter().enumerate() {
        assert_eq!(
            read_file_block(device, superblock, 2, index).unwrap(),
            *image
        );
    }
}

#[test]
fn append_two_blocks_is_one_atomic_transaction() {
    let (mut device, superblock, expected) = setup();
    let data = [[0x31; BLOCK_SIZE], [0x72; BLOCK_SIZE]];

    let (blocks, report) =
        append_file_blocks_journaled(&mut device, &superblock, 2, &data).unwrap();

    assert_eq!(blocks, expected);
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 4);
    assert_new(&mut device, &superblock, &expected, &data);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn every_two_block_append_crash_point_is_old_or_recoverable_new_state() {
    let data = [[0x41; BLOCK_SIZE], [0x82; BLOCK_SIZE]];
    let (mut probe, superblock, expected) = setup();
    probe.arm(None);
    let (_, report) = append_file_blocks_journaled(&mut probe, &superblock, 2, &data).unwrap();
    assert_eq!(report.home_writes, 4);
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 8);
    assert_new(&mut probe, &superblock, &expected, &data);

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, expected) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            append_file_blocks_journaled(&mut device, &superblock, 2, &data)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt multi-block append"
        );
        device.reboot();

        let allocator = load_allocator(&mut device, &superblock).unwrap();
        let raw_owned: Vec<_> = expected
            .iter()
            .map(|block| allocator.is_owned(*block).unwrap())
            .collect();
        let raw_blocks = load_inode_table(&mut device, &superblock)
            .unwrap()
            .into_iter()
            .find(|inode| inode.id == 2)
            .unwrap()
            .blocks;
        let raw_is_old = raw_owned == [false, false] && raw_blocks.is_empty();
        let raw_metadata_is_new = raw_owned == [true, true] && raw_blocks == expected;

        if raw_is_old || raw_metadata_is_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(
                check_device(&mut device).is_err(),
                "crash point {crash_at} exposed partial multi-block ownership/reference metadata that fsck accepted"
            );
        }

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_old(&mut device, &superblock, &expected);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(recovery.home_writes, 4);
            assert_new(&mut device, &superblock, &expected, &data);
        }
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
