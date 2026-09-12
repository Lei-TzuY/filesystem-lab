mod support;

use filesystem_lab::block::BLOCK_SIZE_U64;
use filesystem_lab::filesystem_space::{filesystem_space, FilesystemSpace};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::path_create::create_one_block_file_at_path_journaled;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 8;

fn root_inode(blocks: Vec<u64>) -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks,
    }
}

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(&mut device, &superblock, &[root_inode(Vec::new())]).unwrap();
    (device, superblock)
}

#[test]
fn reports_recovered_empty_and_allocated_space() {
    let (mut device, superblock) = setup();
    let data_blocks = superblock.total_blocks - superblock.reserved_blocks();

    assert_eq!(
        filesystem_space(&mut device, &superblock).unwrap(),
        FilesystemSpace {
            block_size: BLOCK_SIZE_U64,
            total_blocks: superblock.total_blocks,
            reserved_blocks: superblock.reserved_blocks(),
            data_blocks,
            allocated_data_blocks: 0,
            free_data_blocks: data_blocks,
        }
    );

    create_one_block_file_at_path_journaled(
        &mut device,
        &superblock,
        "/payload",
        &[0x5a; 4096],
    )
    .unwrap();

    assert_eq!(
        filesystem_space(&mut device, &superblock).unwrap(),
        FilesystemSpace {
            block_size: BLOCK_SIZE_U64,
            total_blocks: superblock.total_blocks,
            reserved_blocks: superblock.reserved_blocks(),
            data_blocks,
            allocated_data_blocks: 1,
            free_data_blocks: data_blocks - 1,
        }
    );
}

#[test]
fn rejects_allocator_inode_ownership_disagreement() {
    let mut device = CrashDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[root_inode(vec![superblock.reserved_blocks()])],
    )
    .unwrap();

    let error = filesystem_space(&mut device, &superblock).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}
