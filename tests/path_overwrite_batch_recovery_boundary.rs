mod support;

use std::collections::HashSet;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal::JournalEntry;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_overwrite_batch::write_file_blocks_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 10;

fn setup() -> (CrashDevice, Superblock, Vec<u64>) {
    let mut device = CrashDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let blocks = (0..3)
        .map(|_| allocator.allocate().unwrap())
        .collect::<Vec<_>>();
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
                kind: InodeKind::Directory,
                blocks: Vec::new(),
            },
            PersistedInode {
                id: 3,
                kind: InodeKind::File,
                blocks: blocks.clone(),
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
                name: "dir".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 2,
                target: 3,
                name: "file".to_owned(),
            },
        ],
    )
    .unwrap();
    for (index, block) in blocks.iter().copied().enumerate() {
        device
            .write_block(block, &[0x10 + index as u8; BLOCK_SIZE])
            .unwrap();
    }
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks)
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
fn pathname_batch_overwrite_recovers_committed_symlink_before_resolution() {
    let (mut probe, superblock, _) = setup();
    probe.arm(None);
    create_symlink_journaled(&mut probe, &superblock, 1, "file_alias", "/dir/file").unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock, blocks) = setup();
        let allocated_before = load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks();
        let inode_count_before = load_inode_table(&mut device, &superblock).unwrap().len();

        device.arm(Some(crash_at));
        if create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/dir/file").is_ok()
        {
            continue;
        }
        device.reboot();
        if !has_commit(&load_journal_image(&mut device, superblock).unwrap()) {
            continue;
        }
        committed_crash_states += 1;

        let first = [0xa1; BLOCK_SIZE];
        let third = [0xc3; BLOCK_SIZE];
        let report = write_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/file_alias",
            &[(0, first), (2, third)],
        )
        .unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 2);

        let mut actual = [0_u8; BLOCK_SIZE];
        device.read_block(blocks[0], &mut actual).unwrap();
        assert_eq!(actual, first);
        device.read_block(blocks[1], &mut actual).unwrap();
        assert_eq!(actual, [0x11; BLOCK_SIZE]);
        device.read_block(blocks[2], &mut actual).unwrap();
        assert_eq!(actual, third);

        let inodes = load_inode_table(&mut device, &superblock).unwrap();
        let file = inodes.iter().find(|inode| inode.id == 3).unwrap();
        assert_eq!(file.blocks, blocks);
        assert_eq!(inodes.len(), inode_count_before + 1);

        let allocator = load_allocator(&mut device, &superblock).unwrap();
        assert_eq!(allocator.allocated_blocks(), allocated_before + 1);
        assert!(blocks
            .iter()
            .all(|block| allocator.is_owned(*block).unwrap()));

        let namespace = load_directory_table(&mut device, &superblock).unwrap();
        assert!(namespace
            .iter()
            .any(|entry| entry.parent == 1 && entry.name == "file_alias"));
        assert!(namespace
            .iter()
            .any(|entry| entry.parent == 2 && entry.target == 3 && entry.name == "file"));

        assert_unique_ownership(&mut device, &superblock);
        check_device(&mut device).unwrap();
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
