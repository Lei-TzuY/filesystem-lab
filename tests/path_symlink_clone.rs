mod support;

use std::collections::HashSet;
use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode_table::load_inode_table;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_lookup::{
    read_symlink_at_path, resolve_path_without_following_final_symlink,
};
use filesystem_lab::path_symlink::{
    clone_symlink_at_path_journaled, create_symlink_at_path_journaled,
};
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 16;

fn long_target() -> String {
    format!("/{}", "segment".repeat(700))
}

fn setup() -> (CrashDevice, Superblock, String) {
    let mut device = CrashDevice::new(160);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    let target = long_target();
    create_symlink_at_path_journaled(&mut device, &superblock, "/source", &target).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, target)
}

fn inode_blocks(device: &mut CrashDevice, superblock: &Superblock, path: &str) -> Vec<u64> {
    let inode_id = resolve_path_without_following_final_symlink(device, superblock, path).unwrap();
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == inode_id)
        .unwrap()
        .blocks
}

fn assert_unique_ownership(device: &mut CrashDevice, superblock: &Superblock) {
    let allocator = load_allocator(device, superblock).unwrap();
    allocator.validate().unwrap();
    let mut seen = HashSet::new();
    for inode in load_inode_table(device, superblock).unwrap() {
        for block in inode.blocks {
            assert!(seen.insert(block), "duplicate physical block reference {block}");
            assert!(allocator.is_owned(block).unwrap());
        }
    }
}

#[test]
fn clones_multiblock_symlink_target_into_fresh_storage() {
    let (mut device, superblock, target) = setup();
    let source_blocks = inode_blocks(&mut device, &superblock, "/source");
    assert!(source_blocks.len() > 1, "fixture must exercise multi-block SYM2");

    clone_symlink_at_path_journaled(&mut device, &superblock, "/source", "/clone").unwrap();

    assert_eq!(read_symlink_at_path(&mut device, &superblock, "/source").unwrap(), target);
    assert_eq!(read_symlink_at_path(&mut device, &superblock, "/clone").unwrap(), target);
    let clone_blocks = inode_blocks(&mut device, &superblock, "/clone");
    assert_eq!(clone_blocks.len(), source_blocks.len());
    assert!(source_blocks.iter().all(|block| !clone_blocks.contains(block)));
    assert_unique_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
}

#[test]
fn every_symlink_clone_crash_point_recovers_absent_or_complete_destination() {
    let (mut probe, superblock, _) = setup();
    probe.arm(None);
    clone_symlink_at_path_journaled(&mut probe, &superblock, "/source", "/clone").unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock, target) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let source_blocks = inode_blocks(&mut device, &superblock, "/source");

        device.arm(Some(crash_at));
        assert_eq!(
            clone_symlink_at_path_journaled(&mut device, &superblock, "/source", "/clone")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt symlink clone"
        );
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        if recovery.committed_transactions == 0 {
            assert_eq!(
                read_symlink_at_path(&mut device, &superblock, "/clone")
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::NotFound
            );
            assert_eq!(
                load_allocator(&mut device, &superblock).unwrap(),
                allocator_before
            );
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(
                read_symlink_at_path(&mut device, &superblock, "/clone").unwrap(),
                target
            );
            let clone_blocks = inode_blocks(&mut device, &superblock, "/clone");
            assert_eq!(clone_blocks.len(), source_blocks.len());
            assert!(source_blocks.iter().all(|block| !clone_blocks.contains(block)));
            assert_eq!(
                load_allocator(&mut device, &superblock)
                    .unwrap()
                    .allocated_blocks(),
                allocator_before.allocated_blocks() + source_blocks.len() as u64
            );
        }

        assert_eq!(
            read_symlink_at_path(&mut device, &superblock, "/source").unwrap(),
            target
        );
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
}
