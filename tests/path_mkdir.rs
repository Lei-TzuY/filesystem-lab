mod support;

use std::collections::HashSet;
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
use filesystem_lab::path_create::create_directory_at_path_journaled;
use filesystem_lab::path_directory::list_directory_at_path;
use filesystem_lab::path_metadata::metadata_at_path;
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
            inode(3, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "dir"), entry(1, 3, "fileparent")],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn assert_unique_ownership(device: &mut CrashDevice, superblock: &Superblock) {
    let allocator = load_allocator(device, superblock).unwrap();
    allocator.validate().unwrap();
    let inodes = load_inode_table(device, superblock).unwrap();
    let mut seen = HashSet::new();
    for inode in &inodes {
        for block in &inode.blocks {
            assert!(
                seen.insert(*block),
                "duplicate physical block reference {block}"
            );
            assert!(allocator.is_owned(*block).unwrap());
        }
    }
}

#[test]
fn creates_empty_directories_at_direct_symlinked_and_trailing_slash_paths() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();

    let (root_dir, _) =
        create_directory_at_path_journaled(&mut device, &superblock, "/new_root_dir/").unwrap();
    let (nested_dir, _) =
        create_directory_at_path_journaled(&mut device, &superblock, "/dir/new_nested").unwrap();
    let (alias_dir, _) =
        create_directory_at_path_journaled(&mut device, &superblock, "/dir_alias/via_alias/")
            .unwrap();

    for (path, inode_id) in [
        ("/new_root_dir", root_dir),
        ("/dir/new_nested", nested_dir),
        ("/dir/via_alias", alias_dir),
    ] {
        let metadata = metadata_at_path(&mut device, &superblock, path).unwrap();
        assert_eq!(metadata.inode_id, inode_id);
        assert_eq!(metadata.kind, InodeKind::Directory);
        assert_eq!(metadata.logical_blocks, 0);
        assert_eq!(metadata.namespace_references, 1);
        assert!(list_directory_at_path(&mut device, &superblock, path)
            .unwrap()
            .is_empty());
    }

    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_unique_ownership(&mut device, &superblock);
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_invalid_destinations_collisions_and_non_directory_parent_before_publication() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();

    for path in ["relative", "/", "/dir//"] {
        assert_eq!(
            create_directory_at_path_journaled(&mut device, &superblock, path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        create_directory_at_path_journaled(&mut device, &superblock, "/missing/child")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        create_directory_at_path_journaled(&mut device, &superblock, "/dir/")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        create_directory_at_path_journaled(&mut device, &superblock, "/fileparent/child/")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );

    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        inodes_before
    );
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        entries_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_trailing_slash_directory_create_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    create_directory_at_path_journaled(&mut probe, &superblock, "/dir_alias/new_dir/").unwrap();
    let operations = probe.operations();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();

        device.arm(Some(crash_at));
        assert_eq!(
            create_directory_at_path_journaled(&mut device, &superblock, "/dir_alias/new_dir/")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt trailing-slash directory creation"
        );
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let allocator_after = load_allocator(&mut device, &superblock).unwrap();
        let inodes_after = load_inode_table(&mut device, &superblock).unwrap();
        let entries_after = load_directory_table(&mut device, &superblock).unwrap();
        assert_eq!(allocator_after, allocator_before);

        if recovery.committed_transactions == 0 {
            assert_eq!(inodes_after, inodes_before);
            assert_eq!(entries_after, entries_before);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_eq!(inodes_after.len(), inodes_before.len() + 1);
            assert_eq!(entries_after.len(), entries_before.len() + 1);
            let metadata = metadata_at_path(&mut device, &superblock, "/dir/new_dir/").unwrap();
            assert_eq!(metadata.kind, InodeKind::Directory);
            assert_eq!(metadata.logical_blocks, 0);
            assert_eq!(metadata.namespace_references, 1);
            assert!(
                list_directory_at_path(&mut device, &superblock, "/dir/new_dir/")
                    .unwrap()
                    .is_empty()
            );
        }

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
