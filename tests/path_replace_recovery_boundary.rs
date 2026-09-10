mod support;

use std::collections::HashSet;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::BLOCK_SIZE;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_append_batch::append_file_blocks_journaled;
use filesystem_lab::file_data::read_file_block;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal::JournalEntry;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_replace::replace_file_blocks_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 8;
const ORIGINAL: [[u8; BLOCK_SIZE]; 3] =
    [[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE], [0x33; BLOCK_SIZE]];
const REPLACEMENT: [[u8; BLOCK_SIZE]; 2] = [[0xa4; BLOCK_SIZE], [0xb5; BLOCK_SIZE]];

fn inode(id: u64, kind: InodeKind) -> PersistedInode {
    PersistedInode {
        id,
        kind,
        blocks: Vec::new(),
    }
}

fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry {
        parent,
        target,
        name: name.to_owned(),
    }
}

fn setup() -> (CrashDevice, Superblock, Vec<u64>) {
    let mut device = CrashDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[inode(1, InodeKind::Directory), inode(2, InodeKind::File)],
    )
    .unwrap();
    store_directory_table(&mut device, &superblock, &[entry(1, 2, "file")]).unwrap();
    let (original_blocks, _) =
        append_file_blocks_journaled(&mut device, &superblock, 2, &ORIGINAL).unwrap();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, original_blocks)
}

fn has_commit(entries: &[JournalEntry]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry, JournalEntry::Commit { .. }))
}

fn assert_unique_ownership(device: &mut CrashDevice, superblock: &Superblock) {
    let allocator = load_allocator(device, superblock).unwrap();
    allocator.validate().unwrap();
    let inodes = load_inode_table(device, superblock).unwrap();
    let mut seen = HashSet::new();
    for inode in &inodes {
        for block in &inode.blocks {
            assert!(
                seen.insert(*block),
                "duplicate physical block reference {block}"
            );
            assert!(allocator.is_owned(*block).unwrap());
        }
    }
}

#[test]
fn pathname_replacement_recovers_committed_symlink_before_resolution() {
    let (mut probe, superblock, _) = setup();
    probe.arm(None);
    create_symlink_journaled(&mut probe, &superblock, 1, "file_alias", "/file").unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock, original_blocks) = setup();
        let allocated_before = load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks();
        let inode_count_before = load_inode_table(&mut device, &superblock).unwrap().len();

        device.arm(Some(crash_at));
        if create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/file").is_ok() {
            continue;
        }
        device.reboot();
        let durable_journal = load_journal_image(&mut device, superblock).unwrap();
        if !has_commit(&durable_journal) {
            continue;
        }
        committed_crash_states += 1;

        let (new_blocks, displaced, report) = replace_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/file_alias",
            1,
            1,
            &REPLACEMENT,
        )
        .unwrap();

        assert_eq!(report.committed_transactions, 1);
        assert_eq!(displaced, vec![original_blocks[1]]);
        assert_eq!(new_blocks.len(), 2);
        assert_eq!(
            read_file_block(&mut device, &superblock, 2, 0).unwrap(),
            ORIGINAL[0]
        );
        assert_eq!(
            read_file_block(&mut device, &superblock, 2, 1).unwrap(),
            REPLACEMENT[0]
        );
        assert_eq!(
            read_file_block(&mut device, &superblock, 2, 2).unwrap(),
            REPLACEMENT[1]
        );
        assert_eq!(
            read_file_block(&mut device, &superblock, 2, 3).unwrap(),
            ORIGINAL[2]
        );

        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        assert_eq!(allocator_after.allocated_blocks(), allocated_before + 2);
        assert!(!allocator_after.is_owned(original_blocks[1]).unwrap());
        for block in &new_blocks {
            assert!(allocator_after.is_owned(*block).unwrap());
        }
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap().len(),
            inode_count_before + 1
        );
        assert_unique_ownership(&mut device, &superblock);
        check_device(&mut device).unwrap();

        recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }

    assert!(committed_crash_states > 0);
}
