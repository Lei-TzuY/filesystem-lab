mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_unlink::remove_directory_tree_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

#[derive(Clone, Copy)]
struct TreeBlocks {
    private: u64,
    shared: u64,
}

fn setup_tree(with_external_file_link: bool) -> (CrashDevice, Superblock, TreeBlocks) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let private = allocator.allocate().unwrap();
    let shared = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();

    device.write_block(private, &[0x44; BLOCK_SIZE]).unwrap();
    device.write_block(shared, &[0x77; BLOCK_SIZE]).unwrap();
    device.flush().unwrap();

    let inodes = vec![
        PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap(),
        PersistedInode::new(2, InodeKind::Directory, Vec::new()).unwrap(),
        PersistedInode::new(3, InodeKind::Directory, Vec::new()).unwrap(),
        PersistedInode::new(4, InodeKind::File, vec![private]).unwrap(),
        PersistedInode::new(5, InodeKind::File, vec![shared]).unwrap(),
    ];
    let mut entries = vec![
        PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "tree".to_owned(),
        },
        PersistedDirectoryEntry {
            parent: 2,
            target: 3,
            name: "nested".to_owned(),
        },
        PersistedDirectoryEntry {
            parent: 3,
            target: 4,
            name: "private".to_owned(),
        },
        PersistedDirectoryEntry {
            parent: 2,
            target: 5,
            name: "shared".to_owned(),
        },
    ];
    if with_external_file_link {
        entries.push(PersistedDirectoryEntry {
            parent: 1,
            target: 5,
            name: "keep".to_owned(),
        });
    }

    store_inode_table(&mut device, &superblock, &inodes).unwrap();
    store_directory_table(&mut device, &superblock, &entries).unwrap();
    check_device(&mut device).unwrap();

    (device, superblock, TreeBlocks { private, shared })
}

fn read_block(device: &mut CrashDevice, block: u64) -> [u8; BLOCK_SIZE] {
    let mut image = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut image).unwrap();
    image
}

fn assert_removed_state(
    device: &mut CrashDevice,
    superblock: &Superblock,
    blocks: TreeBlocks,
    shared_survives: bool,
) {
    let entries = load_directory_table(device, superblock).unwrap();
    assert!(!entries.iter().any(|entry| entry.name == "tree"));
    assert!(!entries.iter().any(|entry| entry.parent == 2 || entry.parent == 3));

    let inodes = load_inode_table(device, superblock).unwrap();
    assert!(!inodes.iter().any(|inode| matches!(inode.id, 2 | 3 | 4)));
    let allocator = load_allocator(device, superblock).unwrap();
    assert!(!allocator.is_owned(blocks.private).unwrap());

    if shared_survives {
        assert!(entries
            .iter()
            .any(|entry| entry.parent == 1 && entry.target == 5 && entry.name == "keep"));
        assert!(inodes.iter().any(|inode| inode.id == 5));
        assert!(allocator.is_owned(blocks.shared).unwrap());
        assert_eq!(read_block(device, blocks.shared), [0x77; BLOCK_SIZE]);
    } else {
        assert!(!inodes.iter().any(|inode| inode.id == 5));
        assert!(!allocator.is_owned(blocks.shared).unwrap());
    }

    assert!(load_journal_image(device, *superblock).unwrap().is_empty());
    check_device(device).unwrap();
}

#[test]
fn recursive_remove_preserves_externally_linked_file() {
    let (mut device, superblock, blocks) = setup_tree(true);

    let report =
        remove_directory_tree_at_path_journaled(&mut device, &superblock, "/tree").unwrap();

    assert_eq!(report.removed_entries, 4);
    assert_eq!(report.removed_inodes, vec![2, 3, 4]);
    assert_eq!(report.released_blocks, vec![blocks.private]);
    assert_eq!(report.transaction.committed_transactions, 1);
    assert_removed_state(&mut device, &superblock, blocks, true);
}

#[test]
fn recursive_remove_rejects_external_directory_reference_without_mutation() {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap(),
            PersistedInode::new(2, InodeKind::Directory, Vec::new()).unwrap(),
            PersistedInode::new(3, InodeKind::Directory, Vec::new()).unwrap(),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            PersistedDirectoryEntry {
                parent: 1,
                target: 2,
                name: "tree".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 2,
                target: 3,
                name: "nested".to_owned(),
            },
            PersistedDirectoryEntry {
                parent: 1,
                target: 3,
                name: "external".to_owned(),
            },
        ],
    )
    .unwrap();
    check_device(&mut device).unwrap();

    device.arm(None);
    let error =
        remove_directory_tree_at_path_journaled(&mut device, &superblock, "/tree").unwrap_err();

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("external namespace reference"));
    assert_eq!(device.operations(), 0);
}

#[test]
fn recursive_remove_crash_matrix_is_old_or_complete_new_and_converges() {
    let (prepared, superblock, blocks) = setup_tree(false);

    let mut probe = prepared.clone();
    probe.arm(None);
    remove_directory_tree_at_path_journaled(&mut probe, &superblock, "/tree").unwrap();
    let mutation_count = probe.operations();
    assert!(mutation_count > 0);

    for crash_at in 0..mutation_count {
        let mut device = prepared.clone();
        device.arm(Some(crash_at));
        assert!(
            remove_directory_tree_at_path_journaled(&mut device, &superblock, "/tree").is_err(),
            "crash_at={crash_at}"
        );

        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        check_device(&mut device).unwrap();

        let entries = load_directory_table(&mut device, &superblock).unwrap();
        let old_state = entries
            .iter()
            .any(|entry| entry.parent == 1 && entry.target == 2 && entry.name == "tree");
        if old_state {
            let inodes = load_inode_table(&mut device, &superblock).unwrap();
            assert!(inodes.iter().any(|inode| inode.id == 4));
            assert!(inodes.iter().any(|inode| inode.id == 5));
            let allocator = load_allocator(&mut device, &superblock).unwrap();
            assert!(allocator.is_owned(blocks.private).unwrap());
            assert!(allocator.is_owned(blocks.shared).unwrap());

            remove_directory_tree_at_path_journaled(&mut device, &superblock, "/tree").unwrap();
        }

        assert_removed_state(&mut device, &superblock, blocks, false);
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default(),
            "crash_at={crash_at}"
        );
    }
}
