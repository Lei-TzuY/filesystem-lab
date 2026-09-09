mod support;

use std::collections::HashSet;
use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::BLOCK_SIZE;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal::{JournalEntry, JournalLog};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::{load_journal_image, store_journal_image};
use filesystem_lab::path_create::create_one_block_file_at_path_journaled;
use filesystem_lab::path_metadata::metadata_at_path;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 10;

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(128);
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
                kind: InodeKind::Directory,
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
            name: "dir".to_owned(),
        }],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
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
fn committed_create_journal_cannot_be_overwritten_before_recovery() {
    let first_data = [0x31_u8; BLOCK_SIZE];
    let second_data = [0x72_u8; BLOCK_SIZE];
    let (mut probe, superblock) = setup();
    probe.arm(None);
    create_one_block_file_at_path_journaled(&mut probe, &superblock, "/dir/first", &first_data)
        .unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        let result = create_one_block_file_at_path_journaled(
            &mut device,
            &superblock,
            "/dir/first",
            &first_data,
        );
        if result.is_ok() {
            continue;
        }
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Other);
        device.reboot();

        let durable_journal = load_journal_image(&mut device, superblock).unwrap();
        if !has_commit(&durable_journal) {
            continue;
        }
        committed_crash_states += 1;

        let mut replacement = JournalLog::new();
        let txid = replacement.begin().unwrap();
        replacement
            .write(txid, superblock.reserved_blocks(), [0xee_u8; BLOCK_SIZE])
            .unwrap();
        replacement.commit(txid).unwrap();

        assert_eq!(
            store_journal_image(&mut device, superblock, replacement.entries())
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock,
            "crash point {crash_at} must preserve the older committed WAL"
        );
        assert_eq!(
            load_journal_image(&mut device, superblock).unwrap(),
            durable_journal,
            "rejected replacement must not mutate the recovery source"
        );

        let recovered = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(recovered.committed_transactions, 1);
        let first = metadata_at_path(&mut device, &superblock, "/dir/first").unwrap();
        assert_eq!(first.kind, InodeKind::File);
        assert_eq!(first.logical_blocks, 1);
        assert_eq!(first.namespace_references, 1);
        assert_unique_ownership(&mut device, &superblock);
        check_device(&mut device).unwrap();

        create_one_block_file_at_path_journaled(
            &mut device,
            &superblock,
            "/dir/second",
            &second_data,
        )
        .unwrap();
        let second = metadata_at_path(&mut device, &superblock, "/dir/second").unwrap();
        assert_eq!(second.kind, InodeKind::File);
        assert_eq!(second.logical_blocks, 1);
        assert_ne!(first.inode_id, second.inode_id);
        assert_unique_ownership(&mut device, &superblock);
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
    }

    assert!(
        committed_crash_states > 0,
        "create crash matrix must include a durable-commit/pre-complete-home state"
    );
}
