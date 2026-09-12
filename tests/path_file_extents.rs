use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::path_append::append_file_blocks_at_path_journaled;
use filesystem_lab::path_contiguous_create::create_contiguous_file_with_blocks_at_path_journaled;
use filesystem_lab::path_create::create_one_block_file_at_path_journaled;
use filesystem_lab::path_file_extents::{file_extents_at_path, FileExtent};

const JOURNAL_BLOCKS: u64 = 12;

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

fn setup() -> (MemoryDevice, Superblock) {
    let mut device = MemoryDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        }],
    )
    .unwrap();
    (device, superblock)
}

#[test]
fn reports_maximal_physical_runs_for_fragmented_file_mapping() {
    let (mut device, superblock) = setup();
    let initial = [[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE]];
    let (inode_id, _) = create_contiguous_file_with_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/payload",
        &initial,
    )
    .unwrap();
    create_one_block_file_at_path_journaled(
        &mut device,
        &superblock,
        "/gap",
        &[0x33; BLOCK_SIZE],
    )
    .unwrap();
    append_file_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/payload",
        &[[0x44; BLOCK_SIZE]],
    )
    .unwrap();

    let inode = load_inode_table(&mut device, &superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == inode_id)
        .unwrap();
    assert_eq!(inode.blocks.len(), 3);
    assert_eq!(inode.blocks[1], inode.blocks[0] + 1);
    assert!(inode.blocks[2] > inode.blocks[1] + 1);

    assert_eq!(
        file_extents_at_path(&mut device, &superblock, "/payload").unwrap(),
        vec![
            FileExtent {
                logical_start: 0,
                physical_start: inode.blocks[0],
                block_count: 2,
            },
            FileExtent {
                logical_start: 2,
                physical_start: inode.blocks[2],
                block_count: 1,
            },
        ]
    );
}

#[test]
fn reports_one_extent_for_contiguous_file() {
    let (mut device, superblock) = setup();
    let data = [
        [0x51; BLOCK_SIZE],
        [0x62; BLOCK_SIZE],
        [0x73; BLOCK_SIZE],
    ];
    let (inode_id, _) = create_contiguous_file_with_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/contiguous",
        &data,
    )
    .unwrap();
    let inode = load_inode_table(&mut device, &superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == inode_id)
        .unwrap();

    assert_eq!(
        file_extents_at_path(&mut device, &superblock, "/contiguous").unwrap(),
        vec![FileExtent {
            logical_start: 0,
            physical_start: inode.blocks[0],
            block_count: data.len(),
        }]
    );
}

#[test]
fn rejects_non_file_pathname() {
    let (mut device, superblock) = setup();

    assert_eq!(
        file_extents_at_path(&mut device, &superblock, "/")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}
