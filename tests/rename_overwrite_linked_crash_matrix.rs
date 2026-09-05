mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::BlockDevice;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::rename_overwrite_tx::rename_overwrite_linked_file_journaled;
use support::CrashDevice;

fn root() -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }
}

fn setup() -> (CrashDevice, Superblock, u64, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let source_block = allocator.allocate().unwrap();
    let destination_block = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            root(),
            PersistedInode {
                id: 2,
                kind: InodeKind::File,
                blocks: vec![source_block],
            },
            PersistedInode {
                id: 3,
                kind: InodeKind::File,
                blocks: vec![destination_block],
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
            PersistedDirectoryEntry {
                parent: 1,
                target: 3,
                name: "destination-alias".to_owned(),
            },
        ],
    )
    .unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, source_block, destination_block)
}

fn old_entries() -> Vec<PersistedDirectoryEntry> {
    vec![
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
        PersistedDirectoryEntry {
            parent: 1,
            target: 3,
            name: "destination-alias".to_owned(),
        },
    ]
}

fn new_entries() -> Vec<PersistedDirectoryEntry> {
    vec![
        PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "destination".to_owned(),
        },
        PersistedDirectoryEntry {
            parent: 1,
            target: 3,
            name: "destination-alias".to_owned(),
        },
    ]
}

fn replace(device: &mut CrashDevice, superblock: &Superblock) -> io::Result<RecoveryReport> {
    rename_overwrite_linked_file_journaled(device, superblock, 1, "source", 1, "destination")
}

fn assert_metadata_ownership_unchanged(
    device: &mut CrashDevice,
    superblock: &Superblock,
    source_block: u64,
    destination_block: u64,
) {
    let allocator = load_allocator(device, superblock).unwrap();
    assert!(allocator.is_owned(source_block).unwrap());
    assert!(allocator.is_owned(destination_block).unwrap());
    assert_eq!(
        load_inode_table(device, superblock).unwrap(),
        vec![
            root(),
            PersistedInode {
                id: 2,
                kind: InodeKind::File,
                blocks: vec![source_block],
            },
            PersistedInode {
                id: 3,
                kind: InodeKind::File,
                blocks: vec![destination_block],
            },
        ]
    );
}

#[test]
fn linked_destination_replacement_preserves_inode_and_remaining_alias() {
    let (mut device, superblock, source_block, destination_block) = setup();
    let report = replace(&mut device, &superblock).unwrap();
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 1);
    assert_metadata_ownership_unchanged(&mut device, &superblock, source_block, destination_block);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        new_entries()
    );
    check_device(&mut device).unwrap();
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_linked_destination_replacement_crash_is_old_or_recoverable_new() {
    let (mut probe, superblock, source_block, destination_block) = setup();
    probe.arm(None);
    replace(&mut probe, &superblock).unwrap();
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 5);
    assert_metadata_ownership_unchanged(&mut probe, &superblock, source_block, destination_block);
    assert_eq!(
        load_directory_table(&mut probe, &superblock).unwrap(),
        new_entries()
    );

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, source_block, destination_block) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            replace(&mut device, &superblock).unwrap_err().kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt linked rename-overwrite"
        );
        device.reboot();

        assert_metadata_ownership_unchanged(
            &mut device,
            &superblock,
            source_block,
            destination_block,
        );
        let visible_entries = load_directory_table(&mut device, &superblock).unwrap();
        assert!(
            visible_entries == old_entries() || visible_entries == new_entries(),
            "crash point {crash_at} exposed a partial namespace"
        );
        check_device(&mut device).unwrap();

        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if report.committed_transactions == 0 {
            assert_eq!(
                load_directory_table(&mut device, &superblock).unwrap(),
                old_entries()
            );
        } else {
            assert_eq!(report.committed_transactions, 1);
            assert_eq!(report.home_writes, 1);
            assert_eq!(
                load_directory_table(&mut device, &superblock).unwrap(),
                new_entries()
            );
        }
        assert_metadata_ownership_unchanged(
            &mut device,
            &superblock,
            source_block,
            destination_block,
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
