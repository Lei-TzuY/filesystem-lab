use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{read_superblock, DEFAULT_DIRECTORY_BLOCKS, DEFAULT_INODE_BLOCKS};
use filesystem_lab::format_capacity::format_device_for_directory_capacity;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};

#[derive(Debug)]
struct MemoryBlockDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
    flushes: usize,
}

impl MemoryBlockDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0; BLOCK_SIZE]; blocks],
            flushes: 0,
        }
    }
}

impl BlockDevice for MemoryBlockDevice {
    fn block_count(&self) -> u64 {
        u64::try_from(self.blocks.len()).expect("test device length fits u64")
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block index overflow"))?;
        let source = self
            .blocks
            .get(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "block out of range"))?;
        buf.copy_from_slice(source);
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block index overflow"))?;
        let target = self
            .blocks
            .get_mut(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "block out of range"))?;
        target.copy_from_slice(buf);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn directory_capacity_planner_persists_namespace_beyond_default_geometry() {
    const ENTRY_CAPACITY: usize = 120;
    const MAX_NAME_BYTES: usize = 80;

    let mut device = MemoryBlockDevice::new(256);
    let superblock =
        format_device_for_directory_capacity(&mut device, 8, ENTRY_CAPACITY, MAX_NAME_BYTES)
            .unwrap();

    assert_eq!(superblock.journal_blocks, 8);
    assert_eq!(superblock.inode_blocks, DEFAULT_INODE_BLOCKS);
    assert!(superblock.directory_blocks > DEFAULT_DIRECTORY_BLOCKS);
    assert_eq!(read_superblock(&mut device).unwrap(), superblock);

    let mut inodes = vec![PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }];
    let mut entries = Vec::new();
    for index in 0_u64..u64::try_from(ENTRY_CAPACITY).unwrap() {
        let inode_id = index + 2;
        inodes.push(PersistedInode {
            id: inode_id,
            kind: InodeKind::File,
            blocks: Vec::new(),
        });
        let prefix = format!("file-{index:03}-");
        let suffix_len = MAX_NAME_BYTES - prefix.len();
        entries.push(PersistedDirectoryEntry {
            parent: 1,
            target: inode_id,
            name: format!("{prefix}{}", "x".repeat(suffix_len)),
        });
    }

    assert!(entries
        .iter()
        .all(|entry| entry.name.len() == MAX_NAME_BYTES));
    store_inode_table(&mut device, &superblock, &inodes).unwrap();
    store_directory_table(&mut device, &superblock, &entries).unwrap();

    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        entries
    );
    check_device(&mut device).unwrap();
}

#[test]
fn invalid_directory_capacity_is_rejected_before_superblock_publication() {
    for (entry_capacity, max_name_bytes) in [(0, 80), (1, 0), (1, 256), (usize::MAX, 255)] {
        let mut device = MemoryBlockDevice::new(64);
        let error =
            format_device_for_directory_capacity(&mut device, 8, entry_capacity, max_name_bytes)
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(device.blocks[0], [0; BLOCK_SIZE]);
        assert_eq!(device.flushes, 0);
    }
}
