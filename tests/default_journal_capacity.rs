use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::create_tx::store_create_metadata_journaled;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::load_directory_table;
use filesystem_lab::format::{format_device, DEFAULT_JOURNAL_BLOCKS};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::load_inode_table;

#[derive(Debug)]
struct MemoryDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
}

impl MemoryDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0_u8; BLOCK_SIZE]; blocks],
        }
    }
}

impl BlockDevice for MemoryDevice {
    fn block_count(&self) -> u64 {
        u64::try_from(self.blocks.len()).expect("test device size fits u64")
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
        *buf = *self
            .blocks
            .get(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
        *self
            .blocks
            .get_mut(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))? = *buf;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn default_format_can_commit_allocation_inode_and_directory_atomically() {
    let mut device = MemoryDevice::new(32);
    let superblock = format_device(&mut device).unwrap();
    assert_eq!(superblock.journal_blocks, DEFAULT_JOURNAL_BLOCKS);
    assert_eq!(DEFAULT_JOURNAL_BLOCKS, 4);

    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let data_block = allocator.allocate().unwrap();
    let inodes = vec![
        PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        },
        PersistedInode {
            id: 2,
            kind: InodeKind::File,
            blocks: vec![data_block],
        },
    ];
    let entries = vec![PersistedDirectoryEntry {
        parent: 1,
        target: 2,
        name: "file".to_owned(),
    }];

    let report =
        store_create_metadata_journaled(&mut device, &superblock, &allocator, &inodes, &entries)
            .unwrap();

    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 3);
    assert!(load_allocator(&mut device, &superblock)
        .unwrap()
        .is_owned(data_block)
        .unwrap());
    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        entries
    );
    check_device(&mut device).unwrap();
}
