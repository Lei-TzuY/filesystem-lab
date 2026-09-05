mod support;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_data::{read_file_block, write_file_range_journaled};
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

fn write_crossing(device: &mut CrashDevice, superblock: &Superblock) -> io::Result<RecoveryReport> {
    write_file_range_journaled(device, superblock, 2, 0, BLOCK_SIZE - 8, &[0xaa; 16])
}

#[test]
fn cross_block_range_is_one_transaction() {
    let (mut device, superblock) = setup();
    let report = write_crossing(&mut device, &superblock).unwrap();
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 2);
    let first = read_file_block(&mut device, &superblock, 2, 0).unwrap();
    let second = read_file_block(&mut device, &superblock, 2, 1).unwrap();
    assert!(first[..BLOCK_SIZE - 8].iter().all(|b| *b == 0x11));
    assert_eq!(&first[BLOCK_SIZE - 8..], &[0xaa; 8]);
    assert_eq!(&second[..8], &[0xaa; 8]);
    assert!(second[8..].iter().all(|b| *b == 0x22));
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn crossing_past_existing_blocks_is_rejected_before_publication() {
    let (mut device, superblock) = setup();
    let before = read_file_block(&mut device, &superblock, 2, 1).unwrap();
    assert_eq!(
        write_file_range_journaled(&mut device, &superblock, 2, 1, BLOCK_SIZE - 4, &[1; 8])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        read_file_block(&mut device, &superblock, 2, 1).unwrap(),
        before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_cross_block_crash_point_recovers_old_or_complete_new_range() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    write_crossing(&mut probe, &superblock).unwrap();
    let operations = probe.operations();
    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            write_crossing(&mut device, &superblock).unwrap_err().kind(),
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
            assert_eq!(&first[BLOCK_SIZE - 8..], &[0xaa; 8]);
            assert_eq!(&second[..8], &[0xaa; 8]);
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
