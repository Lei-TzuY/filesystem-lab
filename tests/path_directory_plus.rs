use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::path_create::{
    create_directory_at_path_journaled, create_file_with_blocks_at_path_journaled,
};
use filesystem_lab::path_directory_plus::{
    list_directory_with_metadata_at_path, PathDirectoryEntryMetadata,
};
use filesystem_lab::path_hard_link::hard_link_file_at_path_journaled;

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
fn reports_recovered_child_metadata_and_global_reference_counts() {
    let (mut device, superblock) = setup();
    let data = [[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE]];
    let (file_inode, _) = create_file_with_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/payload",
        &data,
    )
    .unwrap();
    let (directory_inode, _) =
        create_directory_at_path_journaled(&mut device, &superblock, "/sub").unwrap();
    hard_link_file_at_path_journaled(&mut device, &superblock, "/payload", "/sub/alias")
        .unwrap();

    assert_eq!(
        list_directory_with_metadata_at_path(&mut device, &superblock, "/").unwrap(),
        vec![
            PathDirectoryEntryMetadata {
                name: "payload".to_owned(),
                inode_id: file_inode,
                kind: InodeKind::File,
                logical_blocks: 2,
                namespace_references: 2,
            },
            PathDirectoryEntryMetadata {
                name: "sub".to_owned(),
                inode_id: directory_inode,
                kind: InodeKind::Directory,
                logical_blocks: 0,
                namespace_references: 1,
            },
        ]
    );

    assert_eq!(
        list_directory_with_metadata_at_path(&mut device, &superblock, "/sub").unwrap(),
        vec![PathDirectoryEntryMetadata {
            name: "alias".to_owned(),
            inode_id: file_inode,
            kind: InodeKind::File,
            logical_blocks: 2,
            namespace_references: 2,
        }]
    );
}

#[test]
fn rejects_non_directory_pathname() {
    let (mut device, superblock) = setup();
    create_file_with_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/payload",
        &[[0x44; BLOCK_SIZE]],
    )
    .unwrap();

    assert_eq!(
        list_directory_with_metadata_at_path(&mut device, &superblock, "/payload")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}
