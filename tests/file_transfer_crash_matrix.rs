mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::file_data::read_file_block;
use filesystem_lab::file_transfer::transfer_file_block_range_journaled;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn setup() -> (CrashDevice, Superblock, [u64; 5]) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let blocks = [
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
    ];
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
                blocks: blocks[..3].to_vec(),
            },
            PersistedInode {
                id: 3,
                kind: InodeKind::File,
                blocks: blocks[3..].to_vec(),
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
                name: "source".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 1,
                target: 3,
                name: "destination".to_owned(),
            },
        ],
    )
    .unwrap();
    for (index, block) in blocks.iter().enumerate() {
        let byte = 0x20 + u8::try_from(index).unwrap();
        device.write_block(*block, &[byte; BLOCK_SIZE]).unwrap();
    }
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks)
}

fn inode_blocks(
    device: &mut CrashDevice,
    superblock: &Superblock,
    inode_id: u64,
) -> Vec<u64> {
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == inode_id)
        .unwrap()
        .blocks
}

fn assert_namespace(device: &mut CrashDevice, superblock: &Superblock) {
    let entries = load_directory_table(device, superblock).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries
        .iter()
        .any(|entry| entry.parent == 1 && entry.target == 2 && entry.name == "source"));
    assert!(entries.iter().any(|entry| {
        entry.parent == 1 && entry.target == 3 && entry.name == "destination"
    }));
}

fn assert_all_owned(device: &mut CrashDevice, superblock: &Superblock, blocks: [u64; 5]) {
    let allocator = load_allocator(device, superblock).unwrap();
    for block in blocks {
        assert!(allocator.is_owned(block).unwrap());
    }
}

fn assert_old(device: &mut CrashDevice, superblock: &Superblock, blocks: [u64; 5]) {
    assert_eq!(inode_blocks(device, superblock, 2), blocks[..3]);
    assert_eq!(inode_blocks(device, superblock, 3), blocks[3..]);
    assert_all_owned(device, superblock, blocks);
    assert_namespace(device, superblock);
}

fn assert_new(device: &mut CrashDevice, superblock: &Superblock, blocks: [u64; 5]) {
    assert_eq!(inode_blocks(device, superblock, 2), vec![blocks[0]]);
    assert_eq!(
        inode_blocks(device, superblock, 3),
        vec![blocks[3], blocks[1], blocks[2], blocks[4]]
    );
    assert_all_owned(device, superblock, blocks);
    assert_eq!(
        read_file_block(device, superblock, 3, 1).unwrap(),
        [0x21; BLOCK_SIZE]
    );
    assert_eq!(
        read_file_block(device, superblock, 3, 2).unwrap(),
        [0x22; BLOCK_SIZE]
    );
    assert_namespace(device, superblock);
}

#[test]
fn transfer_moves_references_without_copying_or_reallocating_blocks() {
    let (mut device, superblock, blocks) = setup();
    let (moved, report) =
        transfer_file_block_range_journaled(&mut device, &superblock, 2, 1, 2, 3, 1).unwrap();
    assert_eq!(moved, vec![blocks[1], blocks[2]]);
    assert_eq!(report.committed_transactions, 1);
    assert!(report.home_writes > 0);
    assert_new(&mut device, &superblock, blocks);
    check_device(&mut device).unwrap();
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn invalid_ranges_and_same_inode_are_rejected_before_publication() {
    let (mut device, superblock, blocks) = setup();
    let empty =
        transfer_file_block_range_journaled(&mut device, &superblock, 2, 0, 0, 3, 0).unwrap_err();
    assert_eq!(empty.kind(), io::ErrorKind::InvalidInput);
    let outside =
        transfer_file_block_range_journaled(&mut device, &superblock, 2, 2, 2, 3, 0).unwrap_err();
    assert_eq!(outside.kind(), io::ErrorKind::InvalidInput);
    let same =
        transfer_file_block_range_journaled(&mut device, &superblock, 2, 0, 1, 2, 0).unwrap_err();
    assert_eq!(same.kind(), io::ErrorKind::InvalidInput);
    assert_old(&mut device, &superblock, blocks);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn allocator_disagreement_is_rejected_before_publication() {
    let (mut device, superblock, blocks) = setup();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    allocator.free(blocks[2]).unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    device.flush().unwrap();

    let error =
        transfer_file_block_range_journaled(&mut device, &superblock, 2, 1, 2, 3, 1).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(inode_blocks(&mut device, &superblock, 2), blocks[..3]);
    assert_eq!(inode_blocks(&mut device, &superblock, 3), blocks[3..]);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_transfer_mutation_crash_point_is_old_or_recoverable_new_state() {
    let (mut probe, superblock, blocks) = setup();
    probe.arm(None);
    let (_, report) =
        transfer_file_block_range_journaled(&mut probe, &superblock, 2, 1, 2, 3, 1).unwrap();
    let expected_home_writes = report.home_writes;
    assert!(expected_home_writes > 0);
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 4);
    assert_new(&mut probe, &superblock, blocks);

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, blocks) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            transfer_file_block_range_journaled(&mut device, &superblock, 2, 1, 2, 3, 1)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();

        let raw_source = inode_blocks(&mut device, &superblock, 2);
        let raw_destination = inode_blocks(&mut device, &superblock, 3);
        let raw_is_old = raw_source == blocks[..3] && raw_destination == blocks[3..];
        let raw_is_new = raw_source == vec![blocks[0]]
            && raw_destination == vec![blocks[3], blocks[1], blocks[2], blocks[4]];
        assert!(
            raw_is_old || raw_is_new,
            "crash point {crash_at} exposed a partial inode-reference transfer"
        );
        check_device(&mut device).unwrap();

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_old(&mut device, &superblock, blocks);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(recovery.home_writes, expected_home_writes);
            assert_new(&mut device, &superblock, blocks);
        }
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());

        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second, RecoveryReport::default());
    }
}
