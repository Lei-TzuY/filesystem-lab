mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::file_append_batch::append_file_blocks_journaled;
use filesystem_lab::file_data::read_file_block;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_replace::replace_file_blocks_at_path_journaled;
use filesystem_lab::path_symlink::create_symlink_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 7;
const ORIGINAL: [[u8; BLOCK_SIZE]; 3] =
    [[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE], [0x33; BLOCK_SIZE]];
const REPLACEMENT: [[u8; BLOCK_SIZE]; 2] = [[0xa4; BLOCK_SIZE], [0xb5; BLOCK_SIZE]];

fn setup() -> (CrashDevice, Superblock, Vec<u64>, Vec<u64>) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
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
            name: "target".to_owned(),
        }],
    )
    .unwrap();
    device.flush().unwrap();

    let (original_blocks, _) =
        append_file_blocks_journaled(&mut device, &superblock, 2, &ORIGINAL).unwrap();
    create_symlink_at_path_journaled(&mut device, &superblock, "/alias", "/target").unwrap();
    check_device(&mut device).unwrap();

    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let expected_new = vec![allocator.allocate().unwrap(), allocator.allocate().unwrap()];
    (device, superblock, original_blocks, expected_new)
}

fn target_blocks(device: &mut CrashDevice, superblock: &Superblock) -> Vec<u64> {
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap()
        .blocks
}

fn assert_old(
    device: &mut CrashDevice,
    superblock: &Superblock,
    original_blocks: &[u64],
    expected_new: &[u64],
) {
    assert_eq!(target_blocks(device, superblock), original_blocks);
    let allocator = load_allocator(device, superblock).unwrap();
    for block in original_blocks {
        assert!(allocator.is_owned(*block).unwrap());
    }
    for block in expected_new {
        assert!(!allocator.is_owned(*block).unwrap());
    }
    for (index, expected) in ORIGINAL.iter().enumerate() {
        assert_eq!(
            read_file_block(device, superblock, 2, index).unwrap(),
            *expected
        );
    }
}

fn assert_new(
    device: &mut CrashDevice,
    superblock: &Superblock,
    original_blocks: &[u64],
    expected_new: &[u64],
) {
    assert_eq!(
        target_blocks(device, superblock),
        vec![
            original_blocks[0],
            expected_new[0],
            expected_new[1],
            original_blocks[2],
        ]
    );
    let allocator = load_allocator(device, superblock).unwrap();
    assert!(allocator.is_owned(original_blocks[0]).unwrap());
    assert!(!allocator.is_owned(original_blocks[1]).unwrap());
    assert!(allocator.is_owned(original_blocks[2]).unwrap());
    for block in expected_new {
        assert!(allocator.is_owned(*block).unwrap());
    }
    assert_eq!(
        read_file_block(device, superblock, 2, 0).unwrap(),
        ORIGINAL[0]
    );
    assert_eq!(
        read_file_block(device, superblock, 2, 1).unwrap(),
        REPLACEMENT[0]
    );
    assert_eq!(
        read_file_block(device, superblock, 2, 2).unwrap(),
        REPLACEMENT[1]
    );
    assert_eq!(
        read_file_block(device, superblock, 2, 3).unwrap(),
        ORIGINAL[2]
    );
}

#[test]
fn pathname_replacement_follows_final_symlink_and_resizes_atomically() {
    let (mut device, superblock, original_blocks, expected_new) = setup();
    let namespace_before = load_directory_table(&mut device, &superblock).unwrap();

    let (new_blocks, displaced, report) = replace_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/alias",
        1,
        1,
        &REPLACEMENT,
    )
    .unwrap();

    assert_eq!(new_blocks, expected_new);
    assert_eq!(displaced, vec![original_blocks[1]]);
    assert_eq!(report.committed_transactions, 1);
    assert_new(&mut device, &superblock, &original_blocks, &expected_new);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        namespace_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn pathname_replacement_rejects_invalid_ranges_without_publication() {
    let (mut device, superblock, original_blocks, expected_new) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inode_before = load_inode_table(&mut device, &superblock).unwrap();
    let namespace_before = load_directory_table(&mut device, &superblock).unwrap();

    for result in [
        replace_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/target",
            1,
            0,
            &REPLACEMENT,
        ),
        replace_file_blocks_at_path_journaled(&mut device, &superblock, "/target", 1, 1, &[]),
        replace_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/target",
            3,
            1,
            &REPLACEMENT,
        ),
        replace_file_blocks_at_path_journaled(&mut device, &superblock, "/", 0, 1, &REPLACEMENT),
    ] {
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        inode_before
    );
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        namespace_before
    );
    assert_old(&mut device, &superblock, &original_blocks, &expected_new);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn every_pathname_replacement_crash_point_is_old_or_recoverable_new_state() {
    let (mut probe, superblock, _, _) = setup();
    probe.arm(None);
    let (_, _, report) = replace_file_blocks_at_path_journaled(
        &mut probe,
        &superblock,
        "/alias",
        1,
        1,
        &REPLACEMENT,
    )
    .unwrap();
    assert_eq!(report.committed_transactions, 1);
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 8);

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, original_blocks, expected_new) = setup();
        let namespace_before = load_directory_table(&mut device, &superblock).unwrap();
        device.arm(Some(crash_at));
        assert_eq!(
            replace_file_blocks_at_path_journaled(
                &mut device,
                &superblock,
                "/alias",
                1,
                1,
                &REPLACEMENT,
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt block replacement"
        );
        device.reboot();

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_old(&mut device, &superblock, &original_blocks, &expected_new);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_new(&mut device, &superblock, &original_blocks, &expected_new);
        }
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            namespace_before
        );
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
