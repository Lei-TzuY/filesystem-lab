mod support;

use std::io;

use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::recovery::recover_journal;
use filesystem_lab::rename_tx::rename_entry_journaled;
use support::CrashDevice;

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

fn old_namespace() -> Vec<PersistedDirectoryEntry> {
    vec![
        entry(1, 2, "left"),
        entry(1, 3, "right"),
        entry(2, 4, "old"),
    ]
}

fn new_namespace() -> Vec<PersistedDirectoryEntry> {
    vec![
        entry(1, 2, "left"),
        entry(1, 3, "right"),
        entry(3, 4, "new"),
    ]
}

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::Directory),
            inode(4, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(&mut device, &superblock, &old_namespace()).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn every_rename_mutation_crash_point_is_old_or_recoverable_new_namespace() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    rename_entry_journaled(&mut probe, &superblock, 2, "old", 3, "new").unwrap();
    let mutation_operations = probe.operations();

    assert!(
        mutation_operations >= 3,
        "rename must cross journal and home durability operations"
    );
    assert_eq!(
        load_directory_table(&mut probe, &superblock).unwrap(),
        new_namespace()
    );
    check_device(&mut probe).unwrap();

    for crash_at in 0..mutation_operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));

        assert_eq!(
            rename_entry_journaled(&mut device, &superblock, 2, "old", 3, "new")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt the rename"
        );

        device.reboot();
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            old_namespace(),
            "crash point {crash_at} exposed a namespace other than the durable old state"
        );
        check_device(&mut device).unwrap();

        let report = recover_journal(&mut device, superblock).unwrap();
        let recovered = load_directory_table(&mut device, &superblock).unwrap();
        if report.committed_transactions == 0 {
            assert_eq!(
                recovered,
                old_namespace(),
                "uncommitted crash point {crash_at} must retain the old namespace"
            );
        } else {
            assert_eq!(report.committed_transactions, 1);
            assert_eq!(report.home_writes, 1);
            assert_eq!(
                recovered,
                new_namespace(),
                "committed crash point {crash_at} must replay the complete new namespace"
            );
        }
        check_device(&mut device).unwrap();

        let second_replay = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(second_replay, report);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            recovered,
            "crash point {crash_at} recovery must be idempotent"
        );
    }
}
