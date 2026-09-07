mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::BLOCK_SIZE;
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
use filesystem_lab::path_append::append_file_blocks_at_path_journaled;
use filesystem_lab::path_clone_splice::{
    clone_file_blocks_splice_at_path_journaled, PathCloneSpliceRange,
};
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::symlink::create_symlink_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 8;
const SOURCE_DATA: [[u8; BLOCK_SIZE]; 2] = [[0x31; BLOCK_SIZE], [0x72; BLOCK_SIZE]];
const DESTINATION_DATA: [[u8; BLOCK_SIZE]; 2] = [[0x19; BLOCK_SIZE], [0x28; BLOCK_SIZE]];

fn inode(id: u64, kind: InodeKind) -> PersistedInode {
    PersistedInode { id, kind, blocks: Vec::new() }
}

fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry { parent, target, name: name.to_owned() }
}

fn range(path: &str, start: usize, block_count: usize) -> PathCloneSpliceRange<'_> {
    PathCloneSpliceRange { path, start, block_count }
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
            inode(4, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 2, "dir"),
            entry(2, 3, "source"),
            entry(2, 4, "destination"),
        ],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "source_alias", "/dir/source").unwrap();
    create_symlink_journaled(
        &mut device,
        &superblock,
        1,
        "destination_alias",
        "/dir/destination",
    )
    .unwrap();
    append_file_blocks_at_path_journaled(&mut device, &superblock, "/dir/source", &SOURCE_DATA)
        .unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/dir/destination",
        &DESTINATION_DATA,
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn run(
    device: &mut CrashDevice,
    superblock: &Superblock,
) -> io::Result<(Vec<u64>, Vec<u64>, RecoveryReport)> {
    clone_file_blocks_splice_at_path_journaled(
        device,
        superblock,
        range("/source_alias", 0, 2),
        range("/destination_alias", 1, 1),
    )
}

#[test]
fn splices_differently_sized_clone_through_symlink_paths() {
    let (mut device, superblock) = setup();
    run(&mut device, &superblock).unwrap();
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/destination", 0, 0, 1).unwrap(),
        vec![0x19]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/destination", 1, 0, 1).unwrap(),
        vec![0x31]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/destination", 2, 0, 1).unwrap(),
        vec![0x72]
    );
    check_device(&mut device).unwrap();
    assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
}

#[test]
fn rejects_invalid_path_clone_splice_without_publication() {
    let (mut device, superblock) = setup();
    let allocator_before = load_allocator(&mut device, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut device, &superblock).unwrap();
    let directory_before = load_directory_table(&mut device, &superblock).unwrap();

    for result in [
        clone_file_blocks_splice_at_path_journaled(
            &mut device,
            &superblock,
            range("/dir", 0, 1),
            range("/dir/destination", 0, 1),
        ),
        clone_file_blocks_splice_at_path_journaled(
            &mut device,
            &superblock,
            range("/dir/source", 0, 0),
            range("/dir/destination", 0, 1),
        ),
        clone_file_blocks_splice_at_path_journaled(
            &mut device,
            &superblock,
            range("/dir/source", 0, 1),
            range("/dir/source", 1, 1),
        ),
        clone_file_blocks_splice_at_path_journaled(
            &mut device,
            &superblock,
            range("/missing", 0, 1),
            range("/dir/destination", 0, 1),
        ),
    ] {
        assert!(matches!(
            result.unwrap_err().kind(),
            io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
        ));
    }

    assert_eq!(load_allocator(&mut device, &superblock).unwrap(), allocator_before);
    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes_before);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        directory_before
    );
    assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
}

#[test]
fn every_path_clone_splice_crash_point_recovers_old_or_complete_new_state() {
    let (mut probe, superblock) = setup();
    let allocator_before = load_allocator(&mut probe, &superblock).unwrap();
    let inodes_before = load_inode_table(&mut probe, &superblock).unwrap();
    let directory_before = load_directory_table(&mut probe, &superblock).unwrap();
    let source_before =
        read_file_range_at_path(&mut probe, &superblock, "/dir/source", 0, 0, 1).unwrap();
    probe.arm(None);
    run(&mut probe, &superblock).unwrap();
    let operations = probe.operations();
    let allocator_after = load_allocator(&mut probe, &superblock).unwrap();
    let inodes_after = load_inode_table(&mut probe, &superblock).unwrap();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(run(&mut device, &superblock).unwrap_err().kind(), io::ErrorKind::Other);
        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        let allocator = load_allocator(&mut device, &superblock).unwrap();
        let inodes = load_inode_table(&mut device, &superblock).unwrap();
        let old = allocator == allocator_before && inodes == inodes_before;
        let new = allocator == allocator_after && inodes == inodes_after;
        assert!(old || new, "crash point {crash_at} recovered a mixed clone-splice state");
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            directory_before
        );
        assert_eq!(
            read_file_range_at_path(&mut device, &superblock, "/dir/source", 0, 0, 1).unwrap(),
            source_before
        );
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }
}
