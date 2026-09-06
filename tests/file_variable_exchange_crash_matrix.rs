mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::BlockDevice;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_exchange::{
    exchange_variable_file_block_ranges_journaled, FileBlockExchangeRange,
};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn setup() -> (CrashDevice, Superblock, [u64; 7]) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let blocks = std::array::from_fn(|_| allocator.allocate().unwrap());
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
                blocks: blocks[..4].to_vec(),
            },
            PersistedInode {
                id: 3,
                kind: InodeKind::File,
                blocks: blocks[4..].to_vec(),
            },
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            PersistedDirectoryEntry {
                parent: 1,
                target: 2,
                name: "left".into(),
            },
            PersistedDirectoryEntry {
                parent: 1,
                target: 3,
                name: "right".into(),
            },
        ],
    )
    .unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks)
}

fn mappings(device: &mut CrashDevice, sb: &Superblock) -> (Vec<u64>, Vec<u64>) {
    let inodes = load_inode_table(device, sb).unwrap();
    let left = inodes
        .iter()
        .find(|inode| inode.id == 2)
        .unwrap()
        .blocks
        .clone();
    let right = inodes
        .iter()
        .find(|inode| inode.id == 3)
        .unwrap()
        .blocks
        .clone();
    (left, right)
}

fn ranges() -> (FileBlockExchangeRange, FileBlockExchangeRange) {
    (
        FileBlockExchangeRange {
            inode: 2,
            start: 1,
            block_count: 2,
        },
        FileBlockExchangeRange {
            inode: 3,
            start: 1,
            block_count: 1,
        },
    )
}

fn expected(blocks: [u64; 7]) -> (Vec<u64>, Vec<u64>) {
    (
        vec![blocks[0], blocks[5], blocks[3]],
        vec![blocks[4], blocks[1], blocks[2], blocks[6]],
    )
}

fn assert_owned(device: &mut CrashDevice, sb: &Superblock, blocks: [u64; 7]) {
    let allocator = load_allocator(device, sb).unwrap();
    for block in blocks {
        assert!(allocator.is_owned(block).unwrap());
    }
}

#[test]
fn variable_exchange_resizes_both_block_vectors_without_reallocation() {
    let (mut device, sb, blocks) = setup();
    let (left, right) = ranges();
    let report =
        exchange_variable_file_block_ranges_journaled(&mut device, &sb, left, right).unwrap();
    assert_eq!(mappings(&mut device, &sb), expected(blocks));
    assert_eq!(report.committed_transactions, 1);
    assert_owned(&mut device, &sb, blocks);
    check_device(&mut device).unwrap();
}

#[test]
fn invalid_variable_exchange_is_rejected_before_publication() {
    let (mut device, sb, blocks) = setup();
    let invalid = [
        (
            FileBlockExchangeRange {
                inode: 2,
                start: 0,
                block_count: 1,
            },
            FileBlockExchangeRange {
                inode: 2,
                start: 1,
                block_count: 1,
            },
        ),
        (
            FileBlockExchangeRange {
                inode: 2,
                start: 0,
                block_count: 0,
            },
            FileBlockExchangeRange {
                inode: 3,
                start: 0,
                block_count: 1,
            },
        ),
        (
            FileBlockExchangeRange {
                inode: 2,
                start: 3,
                block_count: 2,
            },
            FileBlockExchangeRange {
                inode: 3,
                start: 0,
                block_count: 1,
            },
        ),
    ];
    for (left, right) in invalid {
        let error = exchange_variable_file_block_ranges_journaled(&mut device, &sb, left, right)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
    assert_eq!(
        mappings(&mut device, &sb),
        (blocks[..4].to_vec(), blocks[4..].to_vec())
    );
    assert!(load_journal_image(&mut device, sb).unwrap().is_empty());
}

#[test]
fn every_variable_exchange_mutation_crash_point_is_old_or_recoverable_new() {
    let (mut probe, sb, blocks) = setup();
    let (left, right) = ranges();
    probe.arm(None);
    let report =
        exchange_variable_file_block_ranges_journaled(&mut probe, &sb, left, right).unwrap();
    let home_writes = report.home_writes;
    let operations = probe.operations();
    let old = (blocks[..4].to_vec(), blocks[4..].to_vec());
    let new = expected(blocks);

    for crash_at in 0..operations {
        let (mut device, sb, blocks) = setup();
        let (left, right) = ranges();
        device.arm(Some(crash_at));
        assert_eq!(
            exchange_variable_file_block_ranges_journaled(&mut device, &sb, left, right)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        let raw = mappings(&mut device, &sb);
        assert!(
            raw == old || raw == new,
            "crash point {crash_at} exposed partial variable exchange"
        );
        check_device(&mut device).unwrap();
        let recovery = recover_journal_and_checkpoint(&mut device, sb).unwrap();
        if recovery.committed_transactions == 0 {
            assert_eq!(mappings(&mut device, &sb), old);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(recovery.home_writes, home_writes);
            assert_eq!(mappings(&mut device, &sb), new);
        }
        assert_owned(&mut device, &sb, blocks);
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, sb).unwrap().is_empty());
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, sb).unwrap(),
            RecoveryReport::default()
        );
    }
}
