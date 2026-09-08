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
use filesystem_lab::path_collapse::collapse_file_block_range_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

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

fn setup() -> (CrashDevice, Superblock, [u64; 5]) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let blocks = [
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
        allocator.allocate().unwrap(),
    ];
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory, Vec::new()),
            inode(2, InodeKind::Directory, Vec::new()),
            inode(3, InodeKind::File, blocks.to_vec()),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "dir"), entry(2, 3, "file")],
    )
    .unwrap();
    for (index, block) in blocks.iter().enumerate() {
        let byte = 0x20 + u8::try_from(index).unwrap();
        device.write_block(*block, &[byte; BLOCK_SIZE]).unwrap();
    }
    device.flush().unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/dir/file").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dangling", "/missing").unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks)
}

fn file_blocks(device: &mut CrashDevice, superblock: &Superblock) -> Vec<u64> {
    load_inode_table(device, superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 3)
        .unwrap()
        .blocks
}

fn collapse_alias(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    collapse_file_block_range_at_path_journaled(device, superblock, "/file_alias", 2, 2)
}

#[test]
fn collapses_ranges_through_direct_and_symlink_paths() {
    for path in ["/dir/file", "/dir_alias/file", "/file_alias"] {
        let (mut device, superblock, blocks) = setup();
        let (released, report) =
            collapse_file_block_range_at_path_journaled(&mut device, &superblock, path, 2, 2)
                .unwrap();
        assert_eq!(released, vec![blocks[2], blocks[3]]);
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(
            file_blocks(&mut device, &superblock),
            vec![blocks[0], blocks[1], blocks[4]]
        );
        let allocator = load_allocator(&mut device, &superblock).unwrap();
        assert!(!allocator.is_owned(blocks[2]).unwrap());
        assert!(!allocator.is_owned(blocks[3]).unwrap());
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn rejects_invalid_pathname_collapses_without_publication() {
    let (mut device, superblock, _) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let directory_before = load_directory_table(&mut device, &superblock).unwrap();

    assert_eq!(
        collapse_file_block_range_at_path_journaled(&mut device, &superblock, "/dir", 0, 1)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        collapse_file_block_range_at_path_journaled(&mut device, &superblock, "/dangling", 0, 1)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        collapse_file_block_range_at_path_journaled(&mut device, &superblock, "/dir/file", 1, 0)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        collapse_file_block_range_at_path_journaled(&mut device, &superblock, "/dir/file", 4, 2)
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
        directory_before
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_pathname_collapse_crash_point_recovers_old_or_complete_new_state() {
    let (mut expected, superblock, _) = setup();
    let allocator_old = load_allocator(&mut expected, &superblock).unwrap();
    let inodes_old = load_inode_table(&mut expected, &superblock).unwrap();
    let directory_old = load_directory_table(&mut expected, &superblock).unwrap();
    collapse_alias(&mut expected, &superblock).unwrap();
    let allocator_new = load_allocator(&mut expected, &superblock).unwrap();
    let inodes_new = load_inode_table(&mut expected, &superblock).unwrap();

    let (mut probe, superblock, _) = setup();
    probe.arm(None);
    collapse_alias(&mut probe, &superblock).unwrap();
    let operations = probe.operations();
    assert!(operations >= 6);

    for crash_at in 0..operations {
        let (mut device, superblock, _) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            collapse_alias(&mut device, &superblock).unwrap_err().kind(),
            io::ErrorKind::Other
        );
        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let allocator = load_allocator(&mut device, &superblock).unwrap();
        let inodes = load_inode_table(&mut device, &superblock).unwrap();
        assert!(
            (allocator == allocator_old && inodes == inodes_old)
                || (allocator == allocator_new && inodes == inodes_new),
            "crash point {crash_at} recovered a mixed allocator/inode state"
        );
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            directory_old
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
