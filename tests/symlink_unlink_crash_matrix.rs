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

const JOURNAL_BLOCKS: u64 = 6;
const TARGET: &str = "../target/file";

fn root_inode() -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }
}

fn setup_link() -> (CrashDevice, Superblock, u64, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(&mut device, &superblock, &[root_inode()]).unwrap();
    let (inode_id, _) =
        create_symlink_journaled(&mut device, &superblock, 1, "link", TARGET).unwrap();
    let inode = load_inode_table(&mut device, &superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == inode_id)
        .unwrap();
    let block = inode.blocks[0];
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
    (device, superblock, inode_id, block)
}

fn assert_old_state(device: &mut CrashDevice, superblock: &Superblock, inode_id: u64, block: u64) {
    assert!(load_allocator(device, superblock)
        .unwrap()
        .is_owned(block)
        .unwrap());
    assert_eq!(read_symlink(device, superblock, inode_id).unwrap(), TARGET);
    let entries = load_directory_table(device, superblock).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].parent, 1);
    assert_eq!(entries[0].target, inode_id);
    assert_eq!(entries[0].name, "link");
    check_device(device).unwrap();
}

fn assert_new_state(device: &mut CrashDevice, superblock: &Superblock, block: u64) {
    assert!(!load_allocator(device, superblock)
        .unwrap()
        .is_owned(block)
        .unwrap());
    assert_eq!(
        load_inode_table(device, superblock).unwrap(),
        vec![root_inode()]
    );
    assert!(load_directory_table(device, superblock).unwrap().is_empty());
    check_device(device).unwrap();
}

#[test]
fn every_symlink_unlink_mutation_crash_point_is_old_or_recoverable_new_state() {
    let (baseline, superblock, inode_id, block) = setup_link();
    let mut probe = baseline.clone();
    probe.arm(None);
    let report = unlink_symlink_journaled(&mut probe, &superblock, 1, "link").unwrap();
    let mutation_operations = probe.operations();

    assert!(mutation_operations >= 7);
    assert_eq!(report.committed_transactions, 1);
    assert_new_state(&mut probe, &superblock, block);
    assert!(load_journal_image(&mut probe, superblock)
        .unwrap()
        .is_empty());

    for crash_at in 0..mutation_operations {
        let mut device = baseline.clone();
        device.arm(Some(crash_at));
        assert_eq!(
            unlink_symlink_journaled(&mut device, &superblock, 1, "link")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt symlink unlink"
        );
        device.reboot();

        let raw_old = load_allocator(&mut device, &superblock)
            .ok()
            .and_then(|allocator| allocator.is_owned(block).ok())
            == Some(true)
            && read_symlink(&mut device, &superblock, inode_id)
                .is_ok_and(|target| target == TARGET)
            && load_directory_table(&mut device, &superblock).is_ok_and(|entries| {
                entries.len() == 1
                    && entries[0].parent == 1
                    && entries[0].target == inode_id
                    && entries[0].name == "link"
            });
        let raw_new = load_allocator(&mut device, &superblock)
            .ok()
            .and_then(|allocator| allocator.is_owned(block).ok())
            == Some(false)
            && load_inode_table(&mut device, &superblock)
                .is_ok_and(|inodes| inodes == vec![root_inode()])
            && load_directory_table(&mut device, &superblock)
                .is_ok_and(|entries| entries.is_empty());

        if raw_old || raw_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(
                check_device(&mut device).is_err(),
                "crash point {crash_at} exposed a partial unlink state that fsck accepted"
            );
        }

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_old_state(&mut device, &superblock, inode_id, block);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_new_state(&mut device, &superblock, block);
        }
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());

        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second, RecoveryReport::default());
        if recovery.committed_transactions == 0 {
            assert_old_state(&mut device, &superblock, inode_id, block);
        } else {
            assert_new_state(&mut device, &superblock, block);
        }
    }
}
