mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::hard_link_tx::{hard_link_file_journaled, hard_link_symlink_journaled};
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_unlink_dispatch::unlink_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 8;

#[derive(Clone, Copy, Debug)]
enum Case {
    FinalFile,
    NonfinalFile,
    FinalSymlink,
    NonfinalSymlink,
}

impl Case {
    const ALL: [Self; 4] = [
        Self::FinalFile,
        Self::NonfinalFile,
        Self::FinalSymlink,
        Self::NonfinalSymlink,
    ];

    fn path(self) -> &'static str {
        match self {
            Self::FinalFile | Self::NonfinalFile => "/dir/file",
            Self::FinalSymlink | Self::NonfinalSymlink => "/dir/link",
        }
    }
}

fn inode(id: u64, kind: InodeKind, blocks: Vec<u64>) -> PersistedInode {
    PersistedInode { id, kind, blocks }
}

fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry {
        parent,
        target,
        name: name.to_owned(),
    }
}

fn setup(case: Case) -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let file_block = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory, Vec::new()),
            inode(2, InodeKind::Directory, Vec::new()),
            inode(3, InodeKind::File, vec![file_block]),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "dir"), entry(2, 3, "file")],
    )
    .unwrap();
    let (symlink_id, _) =
        create_symlink_journaled(&mut device, &superblock, 2, "link", "../dir/file").unwrap();

    match case {
        Case::NonfinalFile => {
            hard_link_file_journaled(&mut device, &superblock, 2, "file_alias", 3).unwrap();
        }
        Case::NonfinalSymlink => {
            hard_link_symlink_journaled(&mut device, &superblock, 2, "link_alias", symlink_id)
                .unwrap();
        }
        Case::FinalFile | Case::FinalSymlink => {}
    }

    check_device(&mut device).unwrap();
    (device, superblock)
}

fn snapshot(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> (
    filesystem_lab::allocation::BlockAllocator,
    Vec<PersistedInode>,
    Vec<PersistedDirectoryEntry>,
) {
    (
        load_allocator(device, superblock).unwrap(),
        load_inode_table(device, superblock).unwrap(),
        load_directory_table(device, superblock).unwrap(),
    )
}

#[test]
fn dispatches_by_final_inode_kind_and_namespace_reference_count() {
    for case in Case::ALL {
        let (mut device, superblock) = setup(case);
        let before = snapshot(&mut device, &superblock);

        unlink_at_path_journaled(&mut device, &superblock, case.path()).unwrap();
        let after = snapshot(&mut device, &superblock);

        assert_ne!(after, before, "case {case:?} must change durable state");
        assert!(after
            .2
            .iter()
            .all(|entry| !(entry.parent == 2 && entry.name == case.path()[5..])));

        match case {
            Case::FinalFile => {
                assert!(after.1.iter().all(|inode| inode.id != 3));
                assert_eq!(after.0.allocated_blocks() + 1, before.0.allocated_blocks());
            }
            Case::NonfinalFile => {
                assert!(after.1.iter().any(|inode| inode.id == 3));
                assert!(after.2.iter().any(|entry| {
                    entry.parent == 2 && entry.name == "file_alias" && entry.target == 3
                }));
                assert_eq!(after.0, before.0);
            }
            Case::FinalSymlink => {
                assert_eq!(after.0.allocated_blocks() + 1, before.0.allocated_blocks());
            }
            Case::NonfinalSymlink => {
                assert_eq!(after.0, before.0);
                assert!(after
                    .2
                    .iter()
                    .any(|entry| entry.parent == 2 && entry.name == "link_alias"));
            }
        }
        check_device(&mut device).unwrap();
    }
}

#[test]
fn rejects_directories_and_invalid_path_shapes_without_publishing() {
    let (mut device, superblock) = setup(Case::FinalFile);
    let before = snapshot(&mut device, &superblock);

    for path in ["relative", "/", "/dir/", "/dir"] {
        assert_eq!(
            unlink_at_path_journaled(&mut device, &superblock, path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    assert_eq!(snapshot(&mut device, &superblock), before);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn every_dispatch_branch_recovers_to_old_or_complete_new_state_at_every_crash_point() {
    for case in Case::ALL {
        let (mut expected_device, expected_superblock) = setup(case);
        let old_state = snapshot(&mut expected_device, &expected_superblock);
        unlink_at_path_journaled(&mut expected_device, &expected_superblock, case.path()).unwrap();
        let new_state = snapshot(&mut expected_device, &expected_superblock);

        let (mut probe, probe_superblock) = setup(case);
        probe.arm(None);
        unlink_at_path_journaled(&mut probe, &probe_superblock, case.path()).unwrap();
        let operations = probe.operations();

        for crash_at in 0..operations {
            let (mut device, superblock) = setup(case);
            device.arm(Some(crash_at));
            assert_eq!(
                unlink_at_path_journaled(&mut device, &superblock, case.path())
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::Other,
                "case {case:?}, crash_at {crash_at}"
            );
            device.reboot();
            recover_journal_and_checkpoint(&mut device, superblock).unwrap();

            let recovered = snapshot(&mut device, &superblock);
            assert!(
                recovered == old_state || recovered == new_state,
                "case {case:?}, crash_at {crash_at} recovered a partial state"
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
}
