mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::BlockDevice;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_exchange::exchange_file_block_ranges_journaled;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn setup() -> (CrashDevice, Superblock, [u64; 6]) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let blocks = std::array::from_fn(|_| allocator.allocate().unwrap());
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(&mut device, &superblock, &[
        PersistedInode { id: 1, kind: InodeKind::Directory, blocks: Vec::new() },
        PersistedInode { id: 2, kind: InodeKind::File, blocks: blocks[..3].to_vec() },
        PersistedInode { id: 3, kind: InodeKind::File, blocks: blocks[3..].to_vec() },
    ]).unwrap();
    store_directory_table(&mut device, &superblock, &[
        PersistedDirectoryEntry { parent: 1, target: 2, name: "left".into() },
        PersistedDirectoryEntry { parent: 1, target: 3, name: "right".into() },
    ]).unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks)
}

fn mappings(device: &mut CrashDevice, sb: &Superblock) -> (Vec<u64>, Vec<u64>) {
    let inodes = load_inode_table(device, sb).unwrap();
    let left = inodes.iter().find(|inode| inode.id == 2).unwrap().blocks.clone();
    let right = inodes.iter().find(|inode| inode.id == 3).unwrap().blocks.clone();
    (left, right)
}

fn assert_owned(device: &mut CrashDevice, sb: &Superblock, blocks: [u64; 6]) {
    let allocator = load_allocator(device, sb).unwrap();
    for block in blocks { assert!(allocator.is_owned(block).unwrap()); }
}

#[test]
fn exchange_swaps_equal_ranges_without_reallocation() {
    let (mut device, sb, blocks) = setup();
    let report = exchange_file_block_ranges_journaled(&mut device, &sb, 2, 1, 3, 0, 2).unwrap();
    assert_eq!(mappings(&mut device, &sb), (vec![blocks[0], blocks[3], blocks[4]], vec![blocks[1], blocks[2], blocks[5]]));
    assert_eq!(report.committed_transactions, 1);
    assert_owned(&mut device, &sb, blocks);
    check_device(&mut device).unwrap();
}

#[test]
fn invalid_exchange_is_rejected_before_publication() {
    let (mut device, sb, blocks) = setup();
    for error in [
        exchange_file_block_ranges_journaled(&mut device, &sb, 2, 0, 2, 0, 1).unwrap_err(),
        exchange_file_block_ranges_journaled(&mut device, &sb, 2, 0, 3, 0, 0).unwrap_err(),
        exchange_file_block_ranges_journaled(&mut device, &sb, 2, 2, 3, 0, 2).unwrap_err(),
    ] { assert_eq!(error.kind(), io::ErrorKind::InvalidInput); }
    assert_eq!(mappings(&mut device, &sb), (blocks[..3].to_vec(), blocks[3..].to_vec()));
    assert!(load_journal_image(&mut device, sb).unwrap().is_empty());
}

#[test]
fn every_exchange_mutation_crash_point_is_old_or_recoverable_new() {
    let (mut probe, sb, blocks) = setup();
    probe.arm(None);
    let report = exchange_file_block_ranges_journaled(&mut probe, &sb, 2, 1, 3, 0, 2).unwrap();
    let home_writes = report.home_writes;
    let operations = probe.operations();
    let old = (blocks[..3].to_vec(), blocks[3..].to_vec());
    let new = (vec![blocks[0], blocks[3], blocks[4]], vec![blocks[1], blocks[2], blocks[5]]);
    for crash_at in 0..operations {
        let (mut device, sb, blocks) = setup();
        device.arm(Some(crash_at));
        assert_eq!(exchange_file_block_ranges_journaled(&mut device, &sb, 2, 1, 3, 0, 2).unwrap_err().kind(), io::ErrorKind::Other);
        device.reboot();
        let raw = mappings(&mut device, &sb);
        assert!(raw == old || raw == new, "crash point {crash_at} exposed partial exchange");
        check_device(&mut device).unwrap();
        let recovery = recover_journal_and_checkpoint(&mut device, sb).unwrap();
        if recovery.committed_transactions == 0 { assert_eq!(mappings(&mut device, &sb), old); }
        else { assert_eq!(recovery.committed_transactions, 1); assert_eq!(recovery.home_writes, home_writes); assert_eq!(mappings(&mut device, &sb), new); }
        assert_owned(&mut device, &sb, blocks);
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, sb).unwrap().is_empty());
        assert_eq!(recover_journal_and_checkpoint(&mut device, sb).unwrap(), RecoveryReport::default());
    }
}
