mod support;

use std::collections::HashSet;
use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::BLOCK_SIZE;
use filesystem_lab::directory_table::load_directory_table;
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
use filesystem_lab::path_lookup::resolve_path_following_symlinks;
use filesystem_lab::path_rename::rename_at_path_journaled;
use filesystem_lab::recovery::recover_journal;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 10;

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
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

fn assert_renamed_state(
    device: &mut CrashDevice,
    superblock: &Superblock,
    allocated_before: u64,
) {
    assert_eq!(
        resolve_path_following_symlinks(device, superblock, "/source")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    let destination_inode =
        resolve_path_following_symlinks(device, superblock, "/destination").unwrap();
    let inodes = load_inode_table(device, superblock).unwrap();
    let file = inodes
        .iter()
        .find(|inode| inode.id == destination_inode)
        .expect("renamed file inode must remain present");
    assert_eq!(file.kind, InodeKind::File);
    assert_eq!(file.blocks.len(), 1);

    let entries = load_directory_table(device, superblock).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].parent, 1);
    assert_eq!(entries[0].target, destination_inode);
    assert_eq!(entries[0].name, "destination");

    let mut owned = HashSet::new();
    for inode in &inodes {
        for &block in &inode.blocks {
            assert!(
                owned.insert(block),
                "physical block {block} is double-owned"
            );
        }
    }
    assert_eq!(owned.len(), 1);
    assert_eq!(
        load_allocator(device, superblock)
            .unwrap()
            .allocated_blocks(),
        allocated_before + 1
    );
    check_device(device).unwrap();
}

#[test]
fn pathname_rename_recovers_committed_create_before_resolving_and_recomputing() {
    let data = [0x6d_u8; BLOCK_SIZE];
    let (mut probe, superblock) = setup();
    probe.arm(None);
    create_one_block_file_at_path_journaled(&mut probe, &superblock, "/source", &data).unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocated_before = load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks();

        device.arm(Some(crash_at));
        let result =
            create_one_block_file_at_path_journaled(&mut device, &superblock, "/source", &data);
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
            "crash point {crash_at} must preserve the committed create WAL"
        );
        assert_eq!(
            load_journal_image(&mut device, superblock).unwrap(),
            durable_journal,
            "rejected replacement must leave the recovery source unchanged"
        );

        rename_at_path_journaled(&mut device, &superblock, "/source", "/destination").unwrap();
        assert_renamed_state(&mut device, &superblock, allocated_before);

        let rename_journal = load_journal_image(&mut device, superblock).unwrap();
        assert!(
            has_commit(&rename_journal),
            "successful rename keeps its committed WAL until checkpoint"
        );
        let checkpoint_recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(checkpoint_recovery.committed_transactions, 1);
        assert!(checkpoint_recovery.home_writes > 0);
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
        check_device(&mut device).unwrap();

        let second_recovery = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(second_recovery.committed_transactions, 0);
        assert_eq!(second_recovery.home_writes, 0);
        check_device(&mut device).unwrap();
    }

    assert!(
        committed_crash_states > 0,
        "create crash matrix must contain a durable-commit/partial-home state"
    );
}
