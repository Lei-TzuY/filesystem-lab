mod support;

use std::io;

use filesystem_lab::directory_table::load_directory_table;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode_table::load_inode_table;
use filesystem_lab::journal::{JournalEntry, JournalLog};
use filesystem_lab::journal_region::{load_journal_image, store_journal_image};
use filesystem_lab::path_create::create_directory_at_path_journaled;
use filesystem_lab::path_lookup::resolve_path_following_symlinks;
use filesystem_lab::path_unlink::remove_directory_at_path_journaled;
use filesystem_lab::recovery::recover_journal;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 10;

fn setup() -> (CrashDevice, filesystem_lab::format::Superblock) {
    let mut device = CrashDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn has_commit(entries: &[JournalEntry]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry, JournalEntry::Commit { .. }))
}

#[test]
fn pathname_rmdir_recovers_committed_create_before_resolving_and_recomputing() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    create_directory_at_path_journaled(&mut probe, &superblock, "/victim").unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();

        device.arm(Some(crash_at));
        let result = create_directory_at_path_journaled(&mut device, &superblock, "/victim");
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
            .write(txid, superblock.reserved_blocks(), [0xee_u8; filesystem_lab::block::BLOCK_SIZE])
            .unwrap();
        replacement.commit(txid).unwrap();
        assert_eq!(
            store_journal_image(&mut device, superblock, replacement.entries())
                .unwrap_err()
                .kind(),
            io::ErrorKind::WouldBlock,
            "crash point {crash_at} must preserve the committed mkdir WAL"
        );
        assert_eq!(
            load_journal_image(&mut device, superblock).unwrap(),
            durable_journal,
            "rejected replacement must leave the recovery source unchanged"
        );

        remove_directory_at_path_journaled(&mut device, &superblock, "/victim").unwrap();

        assert_eq!(
            resolve_path_following_symlinks(&mut device, &superblock, "/victim")
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(load_inode_table(&mut device, &superblock).unwrap().len(), 1);
        assert!(load_directory_table(&mut device, &superblock).unwrap().is_empty());
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
        "mkdir crash matrix must contain a durable-commit/partial-home state"
    );
}
