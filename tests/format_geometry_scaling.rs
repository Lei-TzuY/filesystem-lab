use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::read_superblock;
use filesystem_lab::format_geometry::format_device_with_metadata_blocks;
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
fn explicit_directory_geometry_supports_namespace_beyond_default_capacity() {
    let mut device = MemoryBlockDevice::new(256);
    let superblock = format_device_with_metadata_blocks(&mut device, 8, 4, 6).unwrap();

    assert_eq!(superblock.journal_blocks, 8);
    assert_eq!(superblock.inode_blocks, 4);
    assert_eq!(superblock.directory_blocks, 6);
    assert_eq!(read_superblock(&mut device).unwrap(), superblock);

    let mut inodes = vec![PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }];
    let mut entries = Vec::new();
    for index in 0_u64..80 {
        let inode_id = index + 2;
        inodes.push(PersistedInode {
            id: inode_id,
            kind: InodeKind::File,
            blocks: Vec::new(),
        });
        entries.push(PersistedDirectoryEntry {
            parent: 1,
            target: inode_id,
            name: format!("file-{index:03}-{}", "x".repeat(96)),
        });
    }

    store_inode_table(&mut device, &superblock, &inodes).unwrap();
    store_directory_table(&mut device, &superblock, &entries).unwrap();

    assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
    assert_eq!(load_directory_table(&mut device, &superblock).unwrap(), entries);
    check_device(&mut device).unwrap();
}

#[test]
fn invalid_explicit_geometry_is_rejected_before_superblock_publication() {
    for geometry in [(0, 4, 6), (8, 0, 6), (8, 4, 0), (200, 100, 100)] {
        let mut device = MemoryBlockDevice::new(64);
        assert!(format_device_with_metadata_blocks(
            &mut device,
            geometry.0,
            geometry.1,
            geometry.2,
        )
        .is_err());
        assert_eq!(device.blocks[0], [0; BLOCK_SIZE]);
        assert_eq!(device.flushes, 0);
    }
}
