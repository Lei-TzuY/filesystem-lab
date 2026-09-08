mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
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
use filesystem_lab::path_insert::insert_file_blocks_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const INSERT_DATA: [[u8; BLOCK_SIZE]; 2] = [[0xa5; BLOCK_SIZE], [0x5a; BLOCK_SIZE]];

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

fn setup() -> (CrashDevice, Superblock, [u64; 3]) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, 12).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let blocks = [
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
        device
            .write_block(*block, &[0x20 + u8::try_from(index).unwrap(); BLOCK_SIZE])
            .unwrap();
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

fn insert_alias(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> io::Result<(Vec<u64>, RecoveryReport)> {
    insert_file_blocks_at_path_journaled(device, superblock, "/file_alias", 1, &INSERT_DATA)
}

#[test]
fn inserts_blocks_through_direct_and_symlink_paths() {
    for path in ["/dir/file", "/dir_alias/file", "/file_alias"] {
        let (mut device, superblock, old_blocks) = setup();
        let (inserted, report) =
            insert_file_blocks_at_path_journaled(&mut device, &superblock, path, 1, &INSERT_DATA)
                .unwrap();
        assert_eq!(inserted.len(), 2);
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(
            file_blocks(&mut device, &superblock),
            vec![
                old_blocks[0],
                inserted[0],
                inserted[1],
                old_blocks[1],
                old_blocks[2]
            ]
        );
        let allocator = load_allocator(&mut device, &superblock).unwrap();
        assert!(allocator.is_owned(inserted[0]).unwrap());
        assert!(allocator.is_owned(inserted[1]).unwrap());
        let mut data = [0_u8; BLOCK_SIZE];
        device.read_block(inserted[0], &mut data).unwrap();
        assert_eq!(data, INSERT_DATA[0]);
        device.read_block(inserted[1], &mut data).unwrap();
        assert_eq!(data, INSERT_DATA[1]);
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
    }
}

#[test]
fn rejects_invalid_pathname_insertions_without_publication() {
    let (mut device, superblock, _) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let directory_before = load_directory_table(&mut device, &superblock).unwrap();

    assert_eq!(
        insert_file_blocks_at_path_journaled(&mut device, &superblock, "/dir", 0, &INSERT_DATA)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        insert_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/dangling",
            0,
            &INSERT_DATA,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        insert_file_blocks_at_path_journaled(&mut device, &superblock, "/dir/file", 0, &[])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        insert_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/dir/file",
            4,
            &INSERT_DATA,
        )
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
fn every_pathname_insert_crash_point_recovers_old_or_complete_new_state() {
    let (mut expected, superblock, _) = setup();
    let allocator_old = load_allocator(&mut expected, &superblock).unwrap();
    let inodes_old = load_inode_table(&mut expected, &superblock).unwrap();
    let directory_old = load_directory_table(&mut expected, &superblock).unwrap();
    insert_alias(&mut expected, &superblock).unwrap();
    let allocator_new = load_allocator(&mut expected, &superblock).unwrap();
    let inodes_new = load_inode_table(&mut expected, &superblock).unwrap();

    let (mut probe, superblock, _) = setup();
    probe.arm(None);
    insert_alias(&mut probe, &superblock).unwrap();
    let operations = probe.operations();
    assert!(operations >= 6);

    for crash_at in 0..operations {
        let (mut device, superblock, _) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            insert_alias(&mut device, &superblock).unwrap_err().kind(),
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
