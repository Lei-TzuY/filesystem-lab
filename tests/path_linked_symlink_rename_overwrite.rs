mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::hard_link_tx::hard_link_symlink_journaled;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_lookup::{
    read_symlink_at_path, resolve_path_without_following_final_symlink,
};
use filesystem_lab::path_rename_overwrite::rename_overwrite_linked_symlink_at_path_journaled;
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

fn setup() -> (CrashDevice, Superblock, u64, u64) {
    let mut device = CrashDevice::new(160);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::Directory),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "src"), entry(1, 3, "dst")],
    )
    .unwrap();

    let (source_inode, _) =
        create_symlink_journaled(&mut device, &superblock, 2, "source", "/source-target").unwrap();
    let (destination_inode, _) =
        create_symlink_journaled(&mut device, &superblock, 3, "target", "/destination-target")
            .unwrap();
    hard_link_symlink_journaled(
        &mut device,
        &superblock,
        3,
        "target_alias",
        destination_inode,
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "src_alias", "/src").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dst_alias", "/dst").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, source_inode, destination_inode)
}

fn assert_new_state(
    device: &mut CrashDevice,
    superblock: &Superblock,
    source_inode: u64,
    destination_inode: u64,
) {
    assert!(
        resolve_path_without_following_final_symlink(device, superblock, "/src/source").is_err()
    );
    assert_eq!(
        resolve_path_without_following_final_symlink(device, superblock, "/dst/target").unwrap(),
        source_inode
    );
    assert_eq!(
        read_symlink_at_path(device, superblock, "/dst/target").unwrap(),
        "/source-target"
    );
    assert_eq!(
        resolve_path_without_following_final_symlink(device, superblock, "/dst/target_alias")
            .unwrap(),
        destination_inode
    );
    assert_eq!(
        read_symlink_at_path(device, superblock, "/dst/target_alias").unwrap(),
        "/destination-target"
    );
}

#[test]
fn replaces_only_selected_alias_and_preserves_destination_inode_and_payload() {
    let (mut device, superblock, source_inode, destination_inode) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();

    rename_overwrite_linked_symlink_at_path_journaled(
        &mut device,
        &superblock,
        "/src_alias/source",
        "/dst_alias/target",
    )
    .unwrap();

    assert_new_state(&mut device, &superblock, source_inode, destination_inode);
    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap(),
        allocator_before
    );
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        inodes_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_singly_linked_destination_without_mutation() {
    let (mut device, superblock, _, destination_inode) = setup();
    let mut entries = load_directory_table(&mut device, &superblock).unwrap();
    entries.retain(|entry| !(entry.parent == 3 && entry.name == "target_alias"));
    store_directory_table(&mut device, &superblock, &entries).unwrap();
    let before = load_directory_table(&mut device, &superblock).unwrap();

    assert_eq!(
        rename_overwrite_linked_symlink_at_path_journaled(
            &mut device,
            &superblock,
            "/src/source",
            "/dst/target",
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        before
    );
    assert!(load_inode_table(&mut device, &superblock)
        .unwrap()
        .iter()
        .any(|inode| inode.id == destination_inode));
    check_device(&mut device).unwrap();
}

#[test]
fn every_linked_symlink_overwrite_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock, _, _) = setup();
    probe.arm(None);
    rename_overwrite_linked_symlink_at_path_journaled(
        &mut probe,
        &superblock,
        "/src_alias/source",
        "/dst_alias/target",
    )
    .unwrap();
    let operations = probe.operations();
    assert!(operations >= 4);

    for crash_at in 0..operations {
        let (mut device, superblock, source_inode, destination_inode) = setup();
        let allocator_before = load_allocator(&mut device, &superblock).unwrap();
        let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
        let entries_before = load_directory_table(&mut device, &superblock).unwrap();
        device.arm(Some(crash_at));
        assert_eq!(
            rename_overwrite_linked_symlink_at_path_journaled(
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
        assert_eq!(
            load_inode_table(&mut device, &superblock).unwrap(),
            inodes_before
        );
        if recovery.committed_transactions == 0 {
            assert_eq!(
                load_directory_table(&mut device, &superblock).unwrap(),
                entries_before
            );
        } else {
            assert_eq!(recovery.committed_transactions, 1);
            assert_new_state(&mut device, &superblock, source_inode, destination_inode);
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
