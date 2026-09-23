mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::BLOCK_SIZE;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::directory_tx::{
    update_directory_table_retained_journaled, RetainedDirectoryUpdateReport,
};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn entry(name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry {
        parent: 1,
        target: 2,
        name: name.to_owned(),
    }
}

fn prepared_filesystem(write_through: bool) -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(64);
    device.write_through = write_through;
    let superblock = format_device_with_journal_blocks(&mut device, 8).unwrap();

    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let data_block = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();

    let root = PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap();
    let file = PersistedInode::new(2, InodeKind::File, vec![data_block]).unwrap();
    store_inode_table(&mut device, &superblock, &[root, file]).unwrap();
    store_directory_table(&mut device, &superblock, &[entry("base")]).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn names(device: &mut CrashDevice, superblock: &Superblock) -> Vec<String> {
    load_directory_table(device, superblock)
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect()
}

fn retain_link(
    device: &mut CrashDevice,
    superblock: &Superblock,
    name: &str,
) -> io::Result<RetainedDirectoryUpdateReport> {
    update_directory_table_retained_journaled(device, superblock, |entries| {
        entries.push(entry(name));
        Ok(())
    })
}

#[test]
fn dependent_retained_directory_updates_plan_against_projected_state() {
    let (mut device, superblock) = prepared_filesystem(false);

    let first = retain_link(&mut device, &superblock, "a").unwrap();
    assert_eq!(
        first,
        RetainedDirectoryUpdateReport {
            retained: RecoveryReport {
                committed_transactions: 1,
                home_writes: 1,
            },
            appended_home_writes: 1,
        }
    );
    assert_eq!(names(&mut device, &superblock), vec!["base"]);

    let second = update_directory_table_retained_journaled(
        &mut device,
        &superblock,
        |entries| {
            if !entries.iter().any(|entry| entry.name == "a") {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "second retained update did not observe first projected update",
                ));
            }
            entries.push(entry("b"));
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        second,
        RetainedDirectoryUpdateReport {
            retained: RecoveryReport {
                committed_transactions: 2,
                home_writes: 2,
            },
            appended_home_writes: 1,
        }
    );

    // Retention deliberately leaves home metadata unchanged until recovery/checkpoint.
    assert_eq!(names(&mut device, &superblock), vec!["base"]);
    assert!(!load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());

    let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    assert_eq!(
        recovery,
        RecoveryReport {
            committed_transactions: 2,
            home_writes: 2,
        }
    );
    assert_eq!(names(&mut device, &superblock), vec!["base", "a", "b"]);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn second_retained_directory_update_crash_converges_to_old_or_new_snapshot() {
    let (mut prepared, superblock) = prepared_filesystem(true);
    retain_link(&mut prepared, &superblock, "a").unwrap();

    let mut probe = prepared.clone();
    probe.arm(None);
    retain_link(&mut probe, &superblock, "b").unwrap();
    let mutation_count = probe.operations();
    assert!(mutation_count >= 4);

    for crash_at in 0..mutation_count {
        let mut device = prepared.clone();
        device.arm(Some(crash_at));
        assert!(
            retain_link(&mut device, &superblock, "b").is_err(),
            "crash_at={crash_at}"
        );

        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert!(
            recovery.committed_transactions == 1 || recovery.committed_transactions == 2,
            "crash_at={crash_at}, recovery={recovery:?}"
        );

        let recovered_names = names(&mut device, &superblock);
        assert!(recovered_names.contains(&"base".to_owned()));
        assert!(recovered_names.contains(&"a".to_owned()));
        assert!(
            recovered_names == vec!["base", "a"]
                || recovered_names == vec!["base", "a", "b"],
            "crash_at={crash_at}, names={recovered_names:?}"
        );
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
        check_device(&mut device).unwrap();
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default(),
            "crash_at={crash_at}"
        );
    }
}

#[test]
fn semantic_bad_retained_directory_update_fails_before_journal_mutation() {
    let (mut device, superblock) = prepared_filesystem(false);
    let home_before = names(&mut device, &superblock);

    device.arm(None);
    let error = update_directory_table_retained_journaled(
        &mut device,
        &superblock,
        |entries| {
            entries.push(PersistedDirectoryEntry {
                parent: 1,
                target: 999,
                name: "dangling".to_owned(),
            });
            Ok(())
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(device.operations(), 0);
    assert_eq!(names(&mut device, &superblock), home_before);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}
