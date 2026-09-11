mod support;

use filesystem_lab::allocation::BlockAllocator;
use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::load_directory_table;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::load_inode_table;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_create::create_empty_file_at_path_journaled;
use filesystem_lab::path_rename_dispatch::rename_posix_at_path_journaled;
use filesystem_lab::path_symlink::create_symlink_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 8;

type State = (BlockAllocator, Vec<PersistedInode>, Vec<PersistedDirectoryEntry>);

fn snapshot(device: &mut CrashDevice, superblock: &Superblock) -> State {
    (
        load_allocator(device, superblock).unwrap(),
        load_inode_table(device, superblock).unwrap(),
        load_directory_table(device, superblock).unwrap(),
    )
}

fn setup_file_over_symlink() -> (CrashDevice, Superblock, u64, u64) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    let (source, _) =
        create_empty_file_at_path_journaled(&mut device, &superblock, "/source").unwrap();
    let (destination, _) = create_symlink_at_path_journaled(
        &mut device,
        &superblock,
        "/destination",
        "/source",
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, source, destination)
}

fn target_for(device: &mut CrashDevice, superblock: &Superblock, name: &str) -> Option<u64> {
    load_directory_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|entry| entry.parent == 1 && entry.name == name)
        .map(|entry| entry.target)
}

#[test]
fn regular_file_can_replace_symbolic_link() {
    let (mut device, superblock, source, replaced) = setup_file_over_symlink();

    rename_posix_at_path_journaled(&mut device, &superblock, "/source", "/destination").unwrap();

    assert_eq!(target_for(&mut device, &superblock, "source"), None);
    assert_eq!(
        target_for(&mut device, &superblock, "destination"),
        Some(source)
    );
    assert!(load_inode_table(&mut device, &superblock)
        .unwrap()
        .iter()
        .all(|inode| inode.id != replaced));
    check_device(&mut device).unwrap();
}

#[test]
fn symbolic_link_can_replace_regular_file() {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    let (source, _) =
        create_symlink_at_path_journaled(&mut device, &superblock, "/source", "/missing")
            .unwrap();
    let (replaced, _) =
        create_empty_file_at_path_journaled(&mut device, &superblock, "/destination").unwrap();

    rename_posix_at_path_journaled(&mut device, &superblock, "/source", "/destination").unwrap();

    assert_eq!(target_for(&mut device, &superblock, "source"), None);
    assert_eq!(
        target_for(&mut device, &superblock, "destination"),
        Some(source)
    );
    assert!(load_inode_table(&mut device, &superblock)
        .unwrap()
        .iter()
        .all(|inode| inode.id != replaced));
    check_device(&mut device).unwrap();
}

#[test]
fn every_cross_kind_overwrite_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock, _, _) = setup_file_over_symlink();
    let old_state = snapshot(&mut probe, &superblock);
    probe.arm(None);
    rename_posix_at_path_journaled(&mut probe, &superblock, "/source", "/destination").unwrap();
    let operations = probe.operations();
    let new_state = snapshot(&mut probe, &superblock);

    for crash_at in 0..operations {
        let (mut device, superblock, _, _) = setup_file_over_symlink();
        device.arm(Some(crash_at));
        assert!(rename_posix_at_path_journaled(
            &mut device,
            &superblock,
            "/source",
            "/destination",
        )
        .is_err());
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        if recovery.committed_transactions == 0 {
            assert_eq!(snapshot(&mut device, &superblock), old_state);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(snapshot(&mut device, &superblock), new_state);
        }
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
