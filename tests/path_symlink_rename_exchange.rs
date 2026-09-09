mod support;

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
use filesystem_lab::path_rename::rename_exchange_symlinks_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::{create_symlink_journaled, read_symlink};
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 8;
const LEFT_TARGET: &str = "/missing/left";
const RIGHT_TARGET: &str = "../missing/right";

fn root_inode() -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }
}

fn setup() -> (CrashDevice, Superblock, u64, u64) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(&mut device, &superblock, &[root_inode()]).unwrap();
    let (left, _) =
        create_symlink_journaled(&mut device, &superblock, 1, "left", LEFT_TARGET).unwrap();
    let (right, _) =
        create_symlink_journaled(&mut device, &superblock, 1, "right", RIGHT_TARGET).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, left, right)
}

fn target_for(device: &mut CrashDevice, superblock: &Superblock, name: &str) -> u64 {
    load_directory_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|entry| entry.parent == 1 && entry.name == name)
        .unwrap()
        .target
}

#[test]
fn exchanges_final_symlink_inodes_without_following_dangling_targets() {
    let (mut device, superblock, left, right) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    rename_exchange_symlinks_at_path_journaled(&mut device, &superblock, "/left", "/right")
        .unwrap();

    assert_eq!(target_for(&mut device, &superblock, "left"), right);
    assert_eq!(target_for(&mut device, &superblock, "right"), left);
    assert_eq!(read_symlink(&mut device, &superblock, left).unwrap(), LEFT_TARGET);
    assert_eq!(
        read_symlink(&mut device, &superblock, right).unwrap(),
        RIGHT_TARGET
    );
    assert_eq!(load_allocator(&mut device, &superblock).unwrap(), allocator_before);
    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes_before);
    check_device(&mut device).unwrap();
}

#[test]
fn every_pathname_symlink_exchange_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock, _, _) = setup();
    probe.arm(None);
    rename_exchange_symlinks_at_path_journaled(&mut probe, &superblock, "/left", "/right")
        .unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock, left, right) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert!(rename_exchange_symlinks_at_path_journaled(
            &mut device,
            &superblock,
            "/left",
            "/right",
        )
        .is_err());
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        assert_eq!(load_allocator(&mut device, &superblock).unwrap(), allocator_before);
        assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes_before);
        if recovery.committed_transactions == 0 {
            assert_eq!(target_for(&mut device, &superblock, "left"), left);
            assert_eq!(target_for(&mut device, &superblock, "right"), right);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(target_for(&mut device, &superblock, "left"), right);
            assert_eq!(target_for(&mut device, &superblock, "right"), left);
        }
        assert_eq!(read_symlink(&mut device, &superblock, left).unwrap(), LEFT_TARGET);
        assert_eq!(
            read_symlink(&mut device, &superblock, right).unwrap(),
            RIGHT_TARGET
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
