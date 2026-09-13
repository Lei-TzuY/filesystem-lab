use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};
use filesystem_lab::filesystem_space::{
    filesystem_free_space_extents, filesystem_free_space_extents_page, filesystem_space,
    FilesystemFreeSpace, FilesystemFreeSpacePage, FilesystemSpace, FreeSpaceExtent,
};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::path_create::create_one_block_file_at_path_journaled;
use filesystem_lab::path_unlink::unlink_file_at_path_journaled;

const JOURNAL_BLOCKS: u64 = 8;

struct MemoryDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
}

impl MemoryDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0; BLOCK_SIZE]; blocks],
        }
    }

    fn block_index(&self, block: u64) -> io::Result<usize> {
        usize::try_from(block)
            .ok()
            .filter(|index| *index < self.blocks.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))
    }
}

impl BlockDevice for MemoryDevice {
    fn block_count(&self) -> u64 {
        u64::try_from(self.blocks.len()).expect("test device block count fits in u64")
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = self.block_index(block)?;
        *buf = self.blocks[index];
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = self.block_index(block)?;
        self.blocks[index] = *buf;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn root_inode(blocks: Vec<u64>) -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks,
    }
}

fn setup() -> (MemoryDevice, Superblock) {
    let mut device = MemoryDevice::new(96);
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

    create_one_block_file_at_path_journaled(&mut device, &superblock, "/payload", &[0x5a; 4096])
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
fn reports_fragmented_free_extents_after_middle_file_is_unlinked() {
    let (mut device, superblock) = setup();
    let first_data_block = superblock.reserved_blocks();

    assert_eq!(
        filesystem_free_space_extents(&mut device, &superblock).unwrap(),
        FilesystemFreeSpace {
            total_free_blocks: superblock.total_blocks - first_data_block,
            largest_extent_blocks: superblock.total_blocks - first_data_block,
            extents: vec![FreeSpaceExtent {
                start_block: first_data_block,
                block_count: superblock.total_blocks - first_data_block,
            }],
        }
    );

    for (path, byte) in [("/a", 0x11), ("/b", 0x22), ("/c", 0x33)] {
        create_one_block_file_at_path_journaled(
            &mut device,
            &superblock,
            path,
            &[byte; BLOCK_SIZE],
        )
        .unwrap();
    }
    unlink_file_at_path_journaled(&mut device, &superblock, "/b").unwrap();

    let tail_start = first_data_block + 3;
    let tail_count = superblock.total_blocks - tail_start;
    assert_eq!(
        filesystem_free_space_extents(&mut device, &superblock).unwrap(),
        FilesystemFreeSpace {
            total_free_blocks: 1 + tail_count,
            largest_extent_blocks: tail_count,
            extents: vec![
                FreeSpaceExtent {
                    start_block: first_data_block + 1,
                    block_count: 1,
                },
                FreeSpaceExtent {
                    start_block: tail_start,
                    block_count: tail_count,
                },
            ],
        }
    );
}

#[test]
fn paginates_fragmented_free_extents_with_exclusive_block_cursor() {
    let (mut device, superblock) = setup();
    let first_data_block = superblock.reserved_blocks();

    for (path, byte) in [
        ("/a", 0x11),
        ("/b", 0x22),
        ("/c", 0x33),
        ("/d", 0x44),
        ("/e", 0x55),
    ] {
        create_one_block_file_at_path_journaled(
            &mut device,
            &superblock,
            path,
            &[byte; BLOCK_SIZE],
        )
        .unwrap();
    }
    unlink_file_at_path_journaled(&mut device, &superblock, "/b").unwrap();
    unlink_file_at_path_journaled(&mut device, &superblock, "/d").unwrap();

    let tail_start = first_data_block + 5;
    let tail_count = superblock.total_blocks - tail_start;
    let total_free_blocks = 2 + tail_count;

    let first = filesystem_free_space_extents_page(&mut device, &superblock, None, 1).unwrap();
    assert_eq!(
        first,
        FilesystemFreeSpacePage {
            total_free_blocks,
            largest_extent_blocks: tail_count,
            extents: vec![FreeSpaceExtent {
                start_block: first_data_block + 1,
                block_count: 1,
            }],
            next_after: Some(first_data_block + 1),
        }
    );

    let second = filesystem_free_space_extents_page(
        &mut device,
        &superblock,
        first.next_after,
        1,
    )
    .unwrap();
    assert_eq!(
        second,
        FilesystemFreeSpacePage {
            total_free_blocks,
            largest_extent_blocks: tail_count,
            extents: vec![FreeSpaceExtent {
                start_block: first_data_block + 3,
                block_count: 1,
            }],
            next_after: Some(first_data_block + 3),
        }
    );

    let third = filesystem_free_space_extents_page(
        &mut device,
        &superblock,
        second.next_after,
        1,
    )
    .unwrap();
    assert_eq!(
        third,
        FilesystemFreeSpacePage {
            total_free_blocks,
            largest_extent_blocks: tail_count,
            extents: vec![FreeSpaceExtent {
                start_block: tail_start,
                block_count: tail_count,
            }],
            next_after: None,
        }
    );
}

#[test]
fn free_space_page_clips_cursor_inside_extent_and_handles_stale_end_cursor() {
    let (mut device, superblock) = setup();
    let first_data_block = superblock.reserved_blocks();
    let total_free_blocks = superblock.total_blocks - first_data_block;
    let cursor = first_data_block + 2;

    assert_eq!(
        filesystem_free_space_extents_page(&mut device, &superblock, Some(cursor), 2).unwrap(),
        FilesystemFreeSpacePage {
            total_free_blocks,
            largest_extent_blocks: total_free_blocks,
            extents: vec![FreeSpaceExtent {
                start_block: cursor + 1,
                block_count: superblock.total_blocks - cursor - 1,
            }],
            next_after: None,
        }
    );

    assert_eq!(
        filesystem_free_space_extents_page(
            &mut device,
            &superblock,
            Some(superblock.total_blocks),
            2,
        )
        .unwrap(),
        FilesystemFreeSpacePage {
            total_free_blocks,
            largest_extent_blocks: total_free_blocks,
            extents: Vec::new(),
            next_after: None,
        }
    );
}

#[test]
fn free_space_page_rejects_zero_limit() {
    let (mut device, superblock) = setup();
    let error = filesystem_free_space_extents_page(&mut device, &superblock, None, 0).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn rejects_allocator_inode_ownership_disagreement() {
    let mut device = MemoryDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[root_inode(vec![superblock.reserved_blocks()])],
    )
    .unwrap();

    let error = filesystem_space(&mut device, &superblock).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let error = filesystem_free_space_extents(&mut device, &superblock).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let error =
        filesystem_free_space_extents_page(&mut device, &superblock, None, 1).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}
