mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_data::read_file_block;
use filesystem_lab::file_overwrite_batch::write_file_blocks_journaled;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const OLD_FIRST: [u8; BLOCK_SIZE] = [0x11; BLOCK_SIZE];
const OLD_SECOND: [u8; BLOCK_SIZE] = [0x22; BLOCK_SIZE];
const NEW_FIRST: [u8; BLOCK_SIZE] = [0xa1; BLOCK_SIZE];
const NEW_SECOND: [u8; BLOCK_SIZE] = [0xb2; BLOCK_SIZE];

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let first = allocator.allocate().unwrap();
    let second = allocator.allocate().unwrap();
    let inodes = vec![
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
    ];
    let entries = vec![PersistedDirectoryEntry {
        parent: 1,
        target: 2,
        name: "file".to_owned(),
    }];

    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(&mut device, &superblock, &inodes).unwrap();
    store_directory_table(&mut device, &superblock, &entries).unwrap();
    device.write_block(first, &OLD_FIRST).unwrap();
    device.write_block(second, &OLD_SECOND).unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn overwrite_pair(device: &mut CrashDevice, superblock: &Superblock) -> io::Result<RecoveryReport> {
    write_file_blocks_journaled(
        device,
        superblock,
        2,
        &[(0, NEW_FIRST), (1, NEW_SECOND)],
    )
}

#[test]
fn multi_block_overwrite_is_one_transaction_and_reuses_checkpointed_journal() {
    let (mut device, superblock) = setup();

    let report = overwrite_pair(&mut device, &superblock).unwrap();
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 2);
    assert_eq!(
        read_file_block(&mut device, &superblock, 2, 0).unwrap(),
        NEW_FIRST
    );
    assert_eq!(
        read_file_block(&mut device, &superblock, 2, 1).unwrap(),
        NEW_SECOND
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();

    assert_eq!(
        overwrite_pair(&mut device, &superblock).unwrap(),
        RecoveryReport::default()
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn multi_block_overwrite_rejects_ambiguous_or_invalid_batches_before_wal_publication() {
    let (mut device, superblock) = setup();

    assert_eq!(
        write_file_blocks_journaled(&mut device, &superblock, 2, &[])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        write_file_blocks_journaled(
            &mut device,
            &superblock,
            2,
            &[(0, NEW_FIRST), (0, NEW_SECOND)],
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        write_file_blocks_journaled(&mut device, &superblock, 2, &[(2, NEW_FIRST)])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        read_file_block(&mut device, &superblock, 2, 0).unwrap(),
        OLD_FIRST
    );
    assert_eq!(
        read_file_block(&mut device, &superblock, 2, 1).unwrap(),
        OLD_SECOND
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_multi_block_overwrite_crash_point_recovers_the_complete_batch() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    overwrite_pair(&mut probe, &superblock).unwrap();
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 7);

    for crash_at in 0..mutation_operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            overwrite_pair(&mut device, &superblock).unwrap_err().kind(),
            io::ErrorKind::Other
        );

        device.reboot();
        check_device(&mut device).unwrap();

        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if report.committed_transactions == 0 {
            assert_eq!(
                read_file_block(&mut device, &superblock, 2, 0).unwrap(),
                OLD_FIRST
            );
            assert_eq!(
                read_file_block(&mut device, &superblock, 2, 1).unwrap(),
                OLD_SECOND
            );
        } else {
            assert_eq!(report.committed_transactions, 1);
            assert_eq!(report.home_writes, 2);
            assert_eq!(
                read_file_block(&mut device, &superblock, 2, 0).unwrap(),
                NEW_FIRST
            );
            assert_eq!(
                read_file_block(&mut device, &superblock, 2, 1).unwrap(),
                NEW_SECOND
            );
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
