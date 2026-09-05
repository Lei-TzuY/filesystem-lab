mod support;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_data::read_file_block;
use filesystem_lab::file_zero_range::zero_file_range_journaled;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use std::io;
use support::CrashDevice;

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let first = allocator.allocate().unwrap();
    let second = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            PersistedInode {
                id: 1,
                kind: InodeKind::Directory,
                blocks: vec![],
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
            name: "file".into(),
        }],
    )
    .unwrap();
    device.write_block(first, &[0x11; BLOCK_SIZE]).unwrap();
    device.write_block(second, &[0x22; BLOCK_SIZE]).unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn zero_crossing(device: &mut CrashDevice, superblock: &Superblock) -> io::Result<RecoveryReport> {
    zero_file_range_journaled(device, superblock, 2, 0, BLOCK_SIZE - 8, 16)
}

#[test]
fn zero_range_crosses_blocks_in_one_transaction() {
    let (mut device, superblock) = setup();
    let report = zero_crossing(&mut device, &superblock).unwrap();
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 2);

    let first = read_file_block(&mut device, &superblock, 2, 0).unwrap();
    let second = read_file_block(&mut device, &superblock, 2, 1).unwrap();
    assert!(first[..BLOCK_SIZE - 8].iter().all(|byte| *byte == 0x11));
    assert!(first[BLOCK_SIZE - 8..].iter().all(|byte| *byte == 0));
    assert!(second[..8].iter().all(|byte| *byte == 0));
    assert!(second[8..].iter().all(|byte| *byte == 0x22));
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn invalid_zero_ranges_are_rejected_before_publication() {
    let (mut device, superblock) = setup();
    assert_eq!(
        zero_file_range_journaled(&mut device, &superblock, 2, 0, 0, 0)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        zero_file_range_journaled(&mut device, &superblock, 2, 1, BLOCK_SIZE - 4, 8)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_zero_range_crash_point_recovers_old_or_complete_zeroed_range() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    zero_crossing(&mut probe, &superblock).unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            zero_crossing(&mut device, &superblock).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        check_device(&mut device).unwrap();
        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        let first = read_file_block(&mut device, &superblock, 2, 0).unwrap();
        let second = read_file_block(&mut device, &superblock, 2, 1).unwrap();
        if report.committed_transactions == 0 {
            assert_eq!(first, [0x11; BLOCK_SIZE]);
            assert_eq!(second, [0x22; BLOCK_SIZE]);
        } else {
            assert!(first[..BLOCK_SIZE - 8].iter().all(|byte| *byte == 0x11));
            assert!(first[BLOCK_SIZE - 8..].iter().all(|byte| *byte == 0));
            assert!(second[..8].iter().all(|byte| *byte == 0));
            assert!(second[8..].iter().all(|byte| *byte == 0x22));
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
