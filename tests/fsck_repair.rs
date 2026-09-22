mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::fsck_repair::repair_orphaned_allocations_journaled;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn inode(id: u64, kind: InodeKind, blocks: Vec<u64>) -> PersistedInode {
    PersistedInode { id, kind, blocks, byte_len: 0 }
}

fn setup_with_orphan() -> (CrashDevice, Superblock, u64, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let owned = allocator.allocate().unwrap();
    let orphan = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();

    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory, Vec::new()),
            inode(2, InodeKind::File, vec![owned]),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "file".to_owned(),
        }],
    )
    .unwrap();

    let error = check_device(&mut device).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains(&format!("allocated block {orphan} has no inode owner")));
    (device, superblock, owned, orphan)
}

fn setup_with_duplicate_owner() -> (CrashDevice, Superblock, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let shared = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();

    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory, Vec::new()),
            inode(2, InodeKind::File, vec![shared]),
            inode(3, InodeKind::File, vec![shared]),
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
                name: "left".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 1,
                target: 3,
                name: "right".to_owned(),
            },
        ],
    )
    .unwrap();

    (device, superblock, shared)
}

#[test]
fn repairs_only_unreferenced_allocations_and_is_idempotent() {
    let (mut device, superblock, owned, orphan) = setup_with_orphan();

    let report = repair_orphaned_allocations_journaled(&mut device, &superblock).unwrap();

    assert_eq!(report.released_blocks, vec![orphan]);
    assert_eq!(report.prior_recovery, RecoveryReport::default());
    assert_eq!(report.repair_transaction.committed_transactions, 1);
    assert!(report.repair_transaction.home_writes > 0);

    let allocator = load_allocator(&mut device, &superblock).unwrap();
    assert!(allocator.is_owned(owned).unwrap());
    assert!(!allocator.is_owned(orphan).unwrap());
    assert_eq!(allocator.allocated_blocks(), 1);
    check_device(&mut device).unwrap();
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());

    device.arm(None);
    let second = repair_orphaned_allocations_journaled(&mut device, &superblock).unwrap();
    assert!(second.released_blocks.is_empty());
    assert_eq!(second.repair_transaction, RecoveryReport::default());
    assert_eq!(device.operations(), 0);
}

#[test]
fn unrelated_corruption_is_rejected_without_mutation() {
    let (mut device, superblock, shared) = setup_with_duplicate_owner();
    let error = check_device(&mut device).unwrap_err();
    assert!(error.to_string().contains("owned by both inode"));

    device.arm(None);
    let error = repair_orphaned_allocations_journaled(&mut device, &superblock).unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("owned by both inode"));
    assert_eq!(device.operations(), 0);
    assert!(load_allocator(&mut device, &superblock)
        .unwrap()
        .is_owned(shared)
        .unwrap());
}

#[test]
fn every_repair_mutation_crash_point_converges_to_a_clean_filesystem() {
    let (prepared, superblock, owned, orphan) = setup_with_orphan();

    let mut probe = prepared.clone();
    probe.arm(None);
    repair_orphaned_allocations_journaled(&mut probe, &superblock).unwrap();
    let mutation_count = probe.operations();
    assert!(mutation_count > 0);

    for crash_at in 0..mutation_count {
        let mut device = prepared.clone();
        device.arm(Some(crash_at));
        assert!(
            repair_orphaned_allocations_journaled(&mut device, &superblock).is_err(),
            "crash_at={crash_at}"
        );

        device.reboot();
        repair_orphaned_allocations_journaled(&mut device, &superblock).unwrap();

        let allocator = load_allocator(&mut device, &superblock).unwrap();
        assert!(allocator.is_owned(owned).unwrap(), "crash_at={crash_at}");
        assert!(!allocator.is_owned(orphan).unwrap(), "crash_at={crash_at}");
        assert_eq!(allocator.allocated_blocks(), 1, "crash_at={crash_at}");
        check_device(&mut device).unwrap();
        assert!(
            load_journal_image(&mut device, superblock)
                .unwrap()
                .is_empty(),
            "crash_at={crash_at}"
        );

        let second = repair_orphaned_allocations_journaled(&mut device, &superblock).unwrap();
        assert!(second.released_blocks.is_empty(), "crash_at={crash_at}");
    }
}
