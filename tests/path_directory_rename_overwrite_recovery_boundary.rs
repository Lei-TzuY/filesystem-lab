mod support;

use std::collections::HashSet;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal::JournalEntry;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_directory_rename_overwrite::rename_overwrite_directory_at_path_journaled;
use filesystem_lab::path_lookup::resolve_path_following_symlinks;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 12;

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
    let mut device = CrashDevice::new(160);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::Directory),
            inode(4, InodeKind::Directory),
            inode(5, InodeKind::Directory),
            inode(6, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 2, "src_parent"),
            entry(1, 3, "dst_parent"),
            entry(2, 4, "source"),
            entry(3, 5, "target"),
            entry(4, 6, "child"),
        ],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn has_commit(entries: &[JournalEntry]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry, JournalEntry::Commit { .. }))
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
fn pathname_directory_rename_overwrite_recovers_committed_parent_symlink_before_resolution() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    create_symlink_journaled(&mut probe, &superblock, 1, "src_alias", "/src_parent").unwrap();
    let operations = probe.operations();
    let mut committed_crash_states = 0;

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        let allocated_before = load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks();

        device.arm(Some(crash_at));
        if create_symlink_journaled(&mut device, &superblock, 1, "src_alias", "/src_parent").is_ok()
        {
            continue;
        }
        device.reboot();
        let durable_journal = load_journal_image(&mut device, superblock).unwrap();
        if !has_commit(&durable_journal) {
            continue;
        }
        committed_crash_states += 1;

        rename_overwrite_directory_at_path_journaled(
            &mut device,
            &superblock,
            "/src_alias/source",
            "/dst_parent/target",
        )
        .unwrap();

        assert!(resolve_path_following_symlinks(
            &mut device,
            &superblock,
            "/src_parent/source",
        )
        .is_err());
        assert_eq!(
            resolve_path_following_symlinks(&mut device, &superblock, "/dst_parent/target")
                .unwrap(),
            4
        );
        assert_eq!(
            resolve_path_following_symlinks(
                &mut device,
                &superblock,
                "/dst_parent/target/child",
            )
            .unwrap(),
            6
        );
        assert_eq!(
            resolve_path_following_symlinks(&mut device, &superblock, "/src_alias").unwrap(),
            2
        );
        assert!(!load_inode_table(&mut device, &superblock)
            .unwrap()
            .iter()
            .any(|inode| inode.id == 5));
        assert_eq!(
            load_allocator(&mut device, &superblock)
                .unwrap()
                .allocated_blocks(),
            allocated_before + 1
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

    assert!(committed_crash_states > 0);
}
