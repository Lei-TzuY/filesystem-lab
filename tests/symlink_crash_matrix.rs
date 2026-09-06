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
use support::CrashDevice;

const SYMLINK_JOURNAL_BLOCKS: u64 = 6;
const TARGET: &str = "../target/file";

fn root_inode() -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }
}

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(64);
    let superblock =
        format_device_with_journal_blocks(&mut device, SYMLINK_JOURNAL_BLOCKS).unwrap();
    store_inode_table(&mut device, &superblock, &[root_inode()]).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn symlink_state(device: &mut CrashDevice, superblock: &Superblock) -> Option<(u64, u64)> {
    let inodes = load_inode_table(device, superblock).ok()?;
    let inode = inodes
        .iter()
        .find(|inode| inode.kind == InodeKind::Symlink)?;
    if inode.blocks.len() != 1 {
        return None;
    }
    Some((inode.id, inode.blocks[0]))
}

fn assert_old_state(device: &mut CrashDevice, superblock: &Superblock) {
    assert_eq!(
        load_inode_table(device, superblock).unwrap(),
        vec![root_inode()]
    );
    assert!(load_directory_table(device, superblock).unwrap().is_empty());
    check_device(device).unwrap();
}

fn assert_new_state(device: &mut CrashDevice, superblock: &Superblock) {
    let (inode_id, block) = symlink_state(device, superblock).expect("symlink inode must exist");
    assert!(load_allocator(device, superblock)
        .unwrap()
        .is_owned(block)
        .unwrap());
    let entries = load_directory_table(device, superblock).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].parent, 1);
    assert_eq!(entries[0].target, inode_id);
    assert_eq!(entries[0].name, "link");
    assert_eq!(read_symlink(device, superblock, inode_id).unwrap(), TARGET);
    check_device(device).unwrap();
}

#[test]
fn every_symlink_mutation_crash_point_is_old_or_recoverable_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    let (inode_id, report) =
        create_symlink_journaled(&mut probe, &superblock, 1, "link", TARGET).unwrap();
    let mutation_operations = probe.operations();

    assert!(mutation_operations >= 8);
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(
        read_symlink(&mut probe, &superblock, inode_id).unwrap(),
        TARGET
    );
    assert_new_state(&mut probe, &superblock);
    assert!(load_journal_image(&mut probe, superblock)
        .unwrap()
        .is_empty());

    for crash_at in 0..mutation_operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));

        assert_eq!(
            create_symlink_journaled(&mut device, &superblock, 1, "link", TARGET)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt symlink creation"
        );

        device.reboot();

        let raw_is_old = load_inode_table(&mut device, &superblock)
            .is_ok_and(|inodes| inodes == vec![root_inode()])
            && load_directory_table(&mut device, &superblock)
                .is_ok_and(|entries| entries.is_empty());
        let raw_is_new = symlink_state(&mut device, &superblock)
            .and_then(|(id, block)| {
                let owned = load_allocator(&mut device, &superblock)
                    .ok()?
                    .is_owned(block)
                    .ok()?;
                let entries = load_directory_table(&mut device, &superblock).ok()?;
                let target_ok =
                    read_symlink(&mut device, &superblock, id).is_ok_and(|target| target == TARGET);
                Some(owned && entries.len() == 1 && entries[0].target == id && target_ok)
            })
            .unwrap_or(false);

        if raw_is_old || raw_is_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(
                check_device(&mut device).is_err(),
                "crash point {crash_at} exposed a partial symlink state that fsck accepted"
            );
        }

        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_old_state(&mut device, &superblock);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_new_state(&mut device, &superblock);
        }
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());

        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second, RecoveryReport::default());
        if recovery.committed_transactions == 0 {
            assert_old_state(&mut device, &superblock);
        } else {
            assert_new_state(&mut device, &superblock);
        }
    }
}
