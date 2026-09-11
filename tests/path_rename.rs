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
use filesystem_lab::path_rename::rename_at_path_journaled;
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
            inode(5, InodeKind::Directory),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 2, "src"),
            entry(1, 3, "dst"),
            entry(2, 4, "file"),
            entry(2, 5, "dir"),
        ],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "src_alias", "/src").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dst_alias", "/dst").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn renames_entry_through_source_and_destination_parent_symlinks() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    rename_at_path_journaled(
        &mut device,
        &superblock,
        "/src_alias/file",
        "/dst_alias/moved",
    )
    .unwrap();

    assert!(resolve_path_following_symlinks(&mut device, &superblock, "/src/file").is_err());
    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/dst/moved").unwrap(),
        4
    );
    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        inodes_before
    );
    check_device(&mut device).unwrap();
}

#[test]
fn trailing_slash_renames_directory_with_directory_only_intent() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    rename_at_path_journaled(
        &mut device,
        &superblock,
        "/src_alias/dir/",
        "/dst_alias/moved_dir/",
    )
    .unwrap();

    assert!(resolve_path_following_symlinks(&mut device, &superblock, "/src/dir").is_err());
    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/dst/moved_dir/").unwrap(),
        5
    );
    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        inodes_before
    );
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_invalid_paths_file_directory_intent_and_destination_collision_before_publication() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let entries_before = load_directory_table(&mut device, &superblock).unwrap();

    for (source, destination) in [
        ("src/file", "/dst/moved"),
        ("/src/file", "dst/moved"),
        ("/", "/dst/moved"),
        ("/src/file", "/"),
        ("/src/file/", "/dst/moved"),
        ("/src/file", "/dst/moved/"),
        ("/src/dir//", "/dst/moved"),
        ("/src/dir", "/dst/moved//"),
        ("/src/file", "/dst_alias"),
    ] {
        assert_eq!(
            rename_at_path_journaled(&mut device, &superblock, source, destination)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

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
fn every_trailing_slash_directory_rename_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    rename_at_path_journaled(
        &mut probe,
        &superblock,
        "/src_alias/dir/",
        "/dst_alias/moved_dir/",
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
            rename_at_path_journaled(
                &mut device,
                &superblock,
                "/src_alias/dir/",
                "/dst_alias/moved_dir/",
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt trailing-slash directory rename"
        );
        device.reboot();
        let recovery = recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        assert_eq!(
            load_allocator(&mut device, &superblock).unwrap(),
            allocator_before
        );
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap(),
            inodes_before
        );
        let entries_after = load_directory_table(&mut device, &superblock).unwrap();
        if recovery.committed_transactions == 0 {
            assert_eq!(entries_after, entries_before);
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert!(resolve_path_following_symlinks(&mut device, &superblock, "/src/dir").is_err());
            assert_eq!(
                resolve_path_following_symlinks(&mut device, &superblock, "/dst/moved_dir/")
                    .unwrap(),
                5
            );
            assert_eq!(entries_after.len(), entries_before.len());
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
