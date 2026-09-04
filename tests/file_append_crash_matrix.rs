mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_data::{append_file_block_journaled, read_file_block};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn setup() -> (CrashDevice, Superblock, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let inodes = vec![
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
    ];
    store_inode_table(&mut device, &superblock, &inodes).unwrap();
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
    let expected_block = allocator.allocate().unwrap();
    (device, superblock, expected_block)
}

fn assert_old(device: &mut CrashDevice, superblock: &Superblock, block: u64) {
    assert!(!load_allocator(device, superblock)
        .unwrap()
        .is_owned(block)
        .unwrap());
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
    block: u64,
    data: &[u8; BLOCK_SIZE],
) {
    assert!(load_allocator(device, superblock)
        .unwrap()
        .is_owned(block)
        .unwrap());
    let inode = load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap();
    assert_eq!(inode.blocks, vec![block]);
    assert_eq!(read_file_block(device, superblock, 2, 0).unwrap(), *data);
}

#[test]
fn append_allocates_references_and_persists_one_complete_block() {
    let (mut device, superblock, expected_block) = setup();
    let data = [0x5a; BLOCK_SIZE];

    let (block, report) = append_file_block_journaled(&mut device, &superblock, 2, data).unwrap();

    assert_eq!(block, expected_block);
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 3);
    assert_new(&mut device, &superblock, block, &data);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn every_append_mutation_crash_point_is_old_or_recoverable_new_state() {
    let data = [0xa7; BLOCK_SIZE];
    let (mut probe, superblock, expected_block) = setup();
    probe.arm(None);
    let (block, report) = append_file_block_journaled(&mut probe, &superblock, 2, data).unwrap();
    assert_eq!(block, expected_block);
    assert_eq!(report.home_writes, 3);
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 7);
    assert_new(&mut probe, &superblock, expected_block, &data);
    assert!(load_journal_image(&mut probe, superblock)
        .unwrap()
        .is_empty());

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, expected_block) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            append_file_block_journaled(&mut device, &superblock, 2, data)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt append"
        );
        device.reboot();

        let raw_owned = load_allocator(&mut device, &superblock)
            .unwrap()
            .is_owned(expected_block)
            .unwrap();
        let raw_blocks = load_inode_table(&mut device, &superblock)
            .unwrap()
            .into_iter()
            .find(|inode| inode.id == 2)
            .unwrap()
            .blocks;
        let raw_is_old = !raw_owned && raw_blocks.is_empty();
        let raw_metadata_is_new = raw_owned && raw_blocks == vec![expected_block];

        if raw_is_old || raw_metadata_is_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(
                check_device(&mut device).is_err(),
                "crash point {crash_at} exposed partial ownership/reference metadata that fsck accepted"
            );
        }

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_old(&mut device, &superblock, expected_block);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(recovery.home_writes, 3);
            assert_new(&mut device, &superblock, expected_block, &data);
        }
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());

        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second, RecoveryReport::default());
    }
}
