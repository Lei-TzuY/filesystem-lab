mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_append_batch::append_file_blocks_journaled;
use filesystem_lab::file_clone_append::clone_file_blocks_append_journaled;
use filesystem_lab::file_data::read_file_block;
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

const CLONE_APPEND_JOURNAL_BLOCKS: u64 = 6;
const SOURCE_DATA: [[u8; BLOCK_SIZE]; 2] = [[0x31; BLOCK_SIZE], [0x72; BLOCK_SIZE]];
const DESTINATION_DATA: [u8; BLOCK_SIZE] = [0x19; BLOCK_SIZE];

fn setup() -> (CrashDevice, Superblock, Vec<u64>, u64, Vec<u64>) {
    let mut device = CrashDevice::new(64);
    let superblock =
        format_device_with_journal_blocks(&mut device, CLONE_APPEND_JOURNAL_BLOCKS).unwrap();
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
            PersistedInode {
                id: 3,
                kind: InodeKind::File,
                blocks: Vec::new(),
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
    device.flush().unwrap();

    let (source_blocks, _) =
        append_file_blocks_journaled(&mut device, &superblock, 2, &SOURCE_DATA).unwrap();
    let (destination_blocks, _) = append_file_blocks_journaled(
        &mut device,
        &superblock,
        3,
        &[DESTINATION_DATA],
    )
    .unwrap();
    let destination_block = destination_blocks[0];
    check_device(&mut device).unwrap();

    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let first = allocator.allocate().unwrap();
    let second = allocator.allocate().unwrap();
    (device, superblock, source_blocks, destination_block, vec![first, second])
}

fn inode_blocks(device: &mut CrashDevice, superblock: &Superblock, inode_id: u64) -> Vec<u64> {
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == inode_id)
        .unwrap()
        .blocks
}

fn assert_source_unchanged(
    device: &mut CrashDevice,
    superblock: &Superblock,
    source_blocks: &[u64],
) {
    assert_eq!(inode_blocks(device, superblock, 2), source_blocks);
    for (index, image) in SOURCE_DATA.iter().enumerate() {
        assert_eq!(read_file_block(device, superblock, 2, index).unwrap(), *image);
    }
}

fn assert_old(
    device: &mut CrashDevice,
    superblock: &Superblock,
    source_blocks: &[u64],
    destination_block: u64,
    expected_new: &[u64],
) {
    assert_source_unchanged(device, superblock, source_blocks);
    assert_eq!(inode_blocks(device, superblock, 3), vec![destination_block]);
    let allocator = load_allocator(device, superblock).unwrap();
    for block in expected_new {
        assert!(!allocator.is_owned(*block).unwrap());
    }
}

fn assert_new(
    device: &mut CrashDevice,
    superblock: &Superblock,
    source_blocks: &[u64],
    destination_block: u64,
    expected_new: &[u64],
) {
    assert_source_unchanged(device, superblock, source_blocks);
    let mut expected_destination = vec![destination_block];
    expected_destination.extend_from_slice(expected_new);
    assert_eq!(inode_blocks(device, superblock, 3), expected_destination);
    let allocator = load_allocator(device, superblock).unwrap();
    for block in expected_new {
        assert!(allocator.is_owned(*block).unwrap());
    }
    assert_eq!(
        read_file_block(device, superblock, 3, 1).unwrap(),
        SOURCE_DATA[0]
    );
    assert_eq!(
        read_file_block(device, superblock, 3, 2).unwrap(),
        SOURCE_DATA[1]
    );
}

#[test]
fn clone_two_blocks_appends_fresh_copies_in_one_transaction() {
    let (mut device, superblock, source_blocks, destination_block, expected_new) = setup();
    let (new_blocks, report) =
        clone_file_blocks_append_journaled(&mut device, &superblock, 2, 0, 2, 3).unwrap();

    assert_eq!(new_blocks, expected_new);
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 4);
    assert_new(
        &mut device,
        &superblock,
        &source_blocks,
        destination_block,
        &expected_new,
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn same_inode_clone_append_uses_source_snapshot() {
    let (mut device, superblock, source_blocks, _, _) = setup();
    let (new_blocks, _) =
        clone_file_blocks_append_journaled(&mut device, &superblock, 2, 0, 2, 2).unwrap();

    let mut expected = source_blocks.clone();
    expected.extend_from_slice(&new_blocks);
    assert_eq!(inode_blocks(&mut device, &superblock, 2), expected);
    assert_eq!(read_file_block(&mut device, &superblock, 2, 2).unwrap(), SOURCE_DATA[0]);
    assert_eq!(read_file_block(&mut device, &superblock, 2, 3).unwrap(), SOURCE_DATA[1]);
    check_device(&mut device).unwrap();
}

#[test]
fn every_clone_append_crash_point_is_old_or_recoverable_new_state() {
    let (mut probe, superblock, source_blocks, destination_block, expected_new) = setup();
    probe.arm(None);
    let (_, report) =
        clone_file_blocks_append_journaled(&mut probe, &superblock, 2, 0, 2, 3).unwrap();
    assert_eq!(report.home_writes, 4);
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 8);
    assert_new(
        &mut probe,
        &superblock,
        &source_blocks,
        destination_block,
        &expected_new,
    );

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, source_blocks, destination_block, expected_new) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            clone_file_blocks_append_journaled(&mut device, &superblock, 2, 0, 2, 3)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt clone append"
        );
        device.reboot();

        let allocator = load_allocator(&mut device, &superblock).unwrap();
        let raw_owned: Vec<_> = expected_new
            .iter()
            .map(|block| allocator.is_owned(*block).unwrap())
            .collect();
        let raw_destination = inode_blocks(&mut device, &superblock, 3);
        let raw_is_old = raw_owned == [false, false] && raw_destination == [destination_block];
        let mut expected_destination = vec![destination_block];
        expected_destination.extend_from_slice(&expected_new);
        let raw_metadata_is_new =
            raw_owned == [true, true] && raw_destination == expected_destination;

        if raw_is_old || raw_metadata_is_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(
                check_device(&mut device).is_err(),
                "crash point {crash_at} exposed partial clone ownership/reference metadata that fsck accepted"
            );
        }

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_old(
                &mut device,
                &superblock,
                &source_blocks,
                destination_block,
                &expected_new,
            );
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(recovery.home_writes, 4);
            assert_new(
                &mut device,
                &superblock,
                &source_blocks,
                destination_block,
                &expected_new,
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
