mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_data::{read_file_block, write_file_block_journaled};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn setup() -> (CrashDevice, Superblock, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let block = allocator.allocate().unwrap();
    let inodes = vec![
        PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        },
        PersistedInode {
            id: 2,
            kind: InodeKind::File,
            blocks: vec![block],
        },
    ];
    let entries = vec![PersistedDirectoryEntry {
        parent: 1,
        target: 2,
        name: "file".to_owned(),
    }];

    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(&mut device, &superblock, &inodes).unwrap();
    store_directory_table(&mut device, &superblock, &entries).unwrap();
    device.write_block(block, &[0x11; BLOCK_SIZE]).unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, block)
}

#[test]
fn journaled_file_block_overwrite_round_trips_and_reuses_checkpointed_journal() {
    let (mut device, superblock, _) = setup();
    let first = [0x5a; BLOCK_SIZE];

    let report = write_file_block_journaled(&mut device, &superblock, 2, 0, first).unwrap();

    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 1);
    assert_eq!(
        read_file_block(&mut device, &superblock, 2, 0).unwrap(),
        first
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();

    assert_eq!(
        write_file_block_journaled(&mut device, &superblock, 2, 0, first).unwrap(),
        RecoveryReport::default()
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());

    let second = [0x6b; BLOCK_SIZE];
    let second_report =
        write_file_block_journaled(&mut device, &superblock, 2, 0, second).unwrap();
    assert_eq!(second_report.committed_transactions, 1);
    assert_eq!(second_report.home_writes, 1);
    assert_eq!(
        read_file_block(&mut device, &superblock, 2, 0).unwrap(),
        second
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn file_block_io_rejects_invalid_targets_without_mutation() {
    let (mut device, superblock, _) = setup();

    assert_eq!(
        write_file_block_journaled(&mut device, &superblock, 1, 0, [0x22; BLOCK_SIZE])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        read_file_block(&mut device, &superblock, 2, 1)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        read_file_block(&mut device, &superblock, 2, 0).unwrap(),
        [0x11; BLOCK_SIZE]
    );
}

#[test]
fn every_file_block_write_and_checkpoint_crash_point_recovers_to_old_or_new_data() {
    let (mut probe, superblock, _) = setup();
    let desired = [0xa5; BLOCK_SIZE];
    probe.arm(None);
    write_file_block_journaled(&mut probe, &superblock, 2, 0, desired).unwrap();
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 6);

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, _) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            write_file_block_journaled(&mut device, &superblock, 2, 0, desired)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );

        device.reboot();
        let raw = read_file_block(&mut device, &superblock, 2, 0).unwrap();
        assert!(raw == [0x11; BLOCK_SIZE] || raw == desired);
        check_device(&mut device).unwrap();

        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if report.committed_transactions == 0 {
            assert_eq!(
                read_file_block(&mut device, &superblock, 2, 0).unwrap(),
                [0x11; BLOCK_SIZE]
            );
        } else {
            assert_eq!(report.committed_transactions, 1);
            assert_eq!(report.home_writes, 1);
            assert_eq!(
                read_file_block(&mut device, &superblock, 2, 0).unwrap(),
                desired
            );
        }
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());

        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second, RecoveryReport::default());
    }
}
