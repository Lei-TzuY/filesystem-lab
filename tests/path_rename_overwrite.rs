mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_lookup::resolve_path_following_symlinks;
use filesystem_lab::path_rename_overwrite::rename_overwrite_file_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 8;

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

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::Directory),
            inode(4, InodeKind::File),
            inode(5, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 2, "src"),
            entry(1, 3, "dst"),
            entry(2, 4, "source"),
            entry(3, 5, "target"),
        ],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "src_alias", "/src").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dst_alias", "/dst").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn overwrites_destination_through_parent_symlinks() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    rename_overwrite_file_at_path_journaled(
        &mut device,
        &superblock,
        "/src_alias/source",
        "/dst_alias/target",
    )
    .unwrap();
    assert!(resolve_path_following_symlinks(&mut device, &superblock, "/src/source").is_err());
    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/dst/target").unwrap(),
        4
    );
    assert!(!load_inode_table(&mut device, &superblock)
        .unwrap()
        .iter()
        .any(|inode| inode.id == 5));
    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_malformed_paths_without_publication() {
    let (mut device, superblock) = setup();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();
    for (source, destination) in [
        ("src/source", "/dst/target"),
        ("/src/source", "dst/target"),
        ("/", "/dst/target"),
        ("/src/source", "/"),
    ] {
        assert_eq!(
            rename_overwrite_file_at_path_journaled(&mut device, &superblock, source, destination)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        entries_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_pathname_overwrite_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    rename_overwrite_file_at_path_journaled(
        &mut probe,
        &superblock,
        "/src_alias/source",
        "/dst_alias/target",
    )
    .unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();
        device.arm(Some(crash_at));
        assert_eq!(
            rename_overwrite_file_at_path_journaled(
                &mut device,
                &superblock,
                "/src_alias/source",
                "/dst_alias/target",
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(
            load_allocator(&mut device, &superblock).unwrap(),
            allocator_before
        );
        if recovery.committed_transactions == 0 {
            assert_eq!(
                load_inode_table(&mut device, &superblock).unwrap(),
                inodes_before
            );
            assert_eq!(
                load_directory_table(&mut device, &superblock).unwrap(),
                entries_before
            );
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert!(
                resolve_path_following_symlinks(&mut device, &superblock, "/src/source").is_err()
            );
            assert_eq!(
                resolve_path_following_symlinks(&mut device, &superblock, "/dst/target").unwrap(),
                4
            );
            assert!(!load_inode_table(&mut device, &superblock)
                .unwrap()
                .iter()
                .any(|inode| inode.id == 5));
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
