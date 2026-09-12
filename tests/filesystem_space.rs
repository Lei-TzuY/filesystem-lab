use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE, BLOCK_SIZE_U64};
use filesystem_lab::filesystem_space::{filesystem_space, FilesystemSpace};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::path_create::create_one_block_file_at_path_journaled;

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
}
