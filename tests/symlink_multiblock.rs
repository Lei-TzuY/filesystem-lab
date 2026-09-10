mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::directory_table::load_directory_table;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::{create_symlink_journaled, read_symlink};
use filesystem_lab::symlink_unlink::unlink_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 12;

fn root_inode() -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }
}

fn long_target() -> String {
    format!("/{}", "segment/".repeat(700))
}

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(&mut device, &superblock, &[root_inode()]).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn symlink_inode(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> Option<PersistedInode> {
    load_inode_table(device, superblock)
        .ok()?
        .into_iter()
        .find(|inode| inode.kind == InodeKind::Symlink)
}

fn assert_new_state(device: &mut CrashDevice, superblock: &Superblock, target: &str) {
    let inode = symlink_inode(device, superblock).expect("multi-block symlink inode must exist");
    assert!(inode.blocks.len() > 1);
    let allocator = load_allocator(device, superblock).unwrap();
    for block in &inode.blocks {
        assert!(allocator.is_owned(*block).unwrap());
    }
    let mut unique = inode.blocks.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), inode.blocks.len());

    let entries = load_directory_table(device, superblock).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].parent, 1);
    assert_eq!(entries[0].target, inode.id);
    assert_eq!(entries[0].name, "long-link");
    assert_eq!(
        read_symlink(device, superblock, inode.id).unwrap(),
        target
    );
    check_device(device).unwrap();
}

fn assert_removed_state(
    device: &mut CrashDevice,
    superblock: &Superblock,
    allocated_before: u64,
) {
    assert_eq!(
        load_allocator(device, superblock)
            .unwrap()
            .allocated_blocks(),
        allocated_before
    );
    assert_eq!(
        load_inode_table(device, superblock).unwrap(),
        vec![root_inode()]
    );
    assert!(load_directory_table(device, superblock).unwrap().is_empty());
    check_device(device).unwrap();
}

#[test]
fn multi_block_symlink_round_trips_and_unlink_frees_every_target_block() {
    let (mut device, superblock) = setup();
    let target = long_target();
    let allocated_before = load_allocator(&mut device, &superblock)
        .unwrap()
        .allocated_blocks();

    let (inode_id, report) =
        create_symlink_journaled(&mut device, &superblock, 1, "long-link", &target).unwrap();
    assert_eq!(report.committed_transactions, 1);
    let inode = symlink_inode(&mut device, &superblock).unwrap();
    assert_eq!(inode.id, inode_id);
    assert!(inode.blocks.len() > 1);
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        allocated_before + u64::try_from(inode.blocks.len()).unwrap()
    );
    assert_new_state(&mut device, &superblock, &target);

    unlink_symlink_journaled(&mut device, &superblock, 1, "long-link").unwrap();
    assert_removed_state(&mut device, &superblock, allocated_before);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_multi_block_symlink_create_crash_point_is_old_or_recoverable_new_state() {
    let target = long_target();
    let (mut probe, superblock) = setup();
    probe.arm(None);
    create_symlink_journaled(&mut probe, &superblock, 1, "long-link", &target).unwrap();
    let mutation_operations = probe.operations();
    assert_new_state(&mut probe, &superblock, &target);

    for crash_at in 0..mutation_operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            create_symlink_journaled(&mut device, &superblock, 1, "long-link", &target)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt multi-block symlink creation"
        );
        device.reboot();

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_eq!(
                load_inode_table(&mut device, &superblock).unwrap(),
                vec![root_inode()]
            );
            assert!(load_directory_table(&mut device, &superblock)
                .unwrap()
                .is_empty());
            check_device(&mut device).unwrap();
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_new_state(&mut device, &superblock, &target);
        }
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());

        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second, RecoveryReport::default());
    }
}

#[test]
fn every_multi_block_symlink_unlink_crash_point_is_old_or_recoverable_removed_state() {
    let target = long_target();
    let (mut probe, superblock) = setup();
    let allocated_before = load_allocator(&mut probe, &superblock)
        .unwrap()
        .allocated_blocks();
    create_symlink_journaled(&mut probe, &superblock, 1, "long-link", &target).unwrap();
    probe.arm(None);
    unlink_symlink_journaled(&mut probe, &superblock, 1, "long-link").unwrap();
    let mutation_operations = probe.operations();
    assert_removed_state(&mut probe, &superblock, allocated_before);

    for crash_at in 0..mutation_operations {
        let (mut device, superblock) = setup();
        let allocated_before = load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks();
        create_symlink_journaled(&mut device, &superblock, 1, "long-link", &target).unwrap();
        device.arm(Some(crash_at));
        assert_eq!(
            unlink_symlink_journaled(&mut device, &superblock, 1, "long-link")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt multi-block symlink unlink"
        );
        device.reboot();

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_new_state(&mut device, &superblock, &target);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_removed_state(&mut device, &superblock, allocated_before);
        }
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());

        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second, RecoveryReport::default());
        if recovery.committed_transactions == 0 {
            assert_new_state(&mut device, &superblock, &target);
        } else {
            assert_removed_state(&mut device, &superblock, allocated_before);
        }
    }
}
