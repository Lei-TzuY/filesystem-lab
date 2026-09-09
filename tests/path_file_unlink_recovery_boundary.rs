mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::BLOCK_SIZE;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal::{JournalEntry, JournalLog};
use filesystem_lab::journal_region::{load_journal_image, store_journal_image};
use filesystem_lab::path_create::create_one_block_file_at_path_journaled;
use filesystem_lab::path_lookup::resolve_path_following_symlinks;
use filesystem_lab::path_unlink::unlink_file_at_path_journaled;
use filesystem_lab::recovery::recover_journal;
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

#[test]
fn pathname_unlink_recovers_committed_create_before_resolving_and_recomputing() {
    let data = [0x5a_u8; BLOCK_SIZE];
    let (mut probe, superblock) = setup();
    probe.arm(None);
    create_one_block_file_at_path_journaled(&mut probe, &superblock, "/dir/victim", &data).unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocated_before = load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks();

        device.arm(Some(crash_at));
        let result =
            create_one_block_file_at_path_journaled(&mut device, &superblock, "/dir/victim", &data);
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

        unlink_file_at_path_journaled(&mut device, &superblock, "/dir/victim").unwrap();

        assert_eq!(
            resolve_path_following_symlinks(&mut device, &superblock, "/dir/victim")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(load_inode_table(&mut device, &superblock)
            .unwrap()
            .iter()
            .all(|inode| inode.id <= 2));
        assert!(load_directory_table(&mut device, &superblock)
            .unwrap()
            .iter()
            .all(|entry| !(entry.parent == 2 && entry.name == "victim")));
        assert_eq!(
            load_allocator(&mut device, &superblock)
                .unwrap()
                .allocated_blocks(),
            allocated_before
        );
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());

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
