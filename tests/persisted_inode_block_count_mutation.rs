use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::format_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};

#[derive(Debug)]
struct MemoryDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
}

impl MemoryDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0; BLOCK_SIZE]; blocks],
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
fn block_count_mutations_round_trip_through_durable_inode_table() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();

    let root = PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap();
    let mut file = PersistedInode::new(2, InodeKind::File, vec![20, 21, 22]).unwrap();

    let displaced = file.replace_block_range(3..3, &[30, 31]).unwrap();
    assert!(displaced.is_empty());
    assert_eq!(file.blocks, vec![20, 21, 22, 30, 31]);

    let displaced = file.replace_block_range(1..1, &[40]).unwrap();
    assert!(displaced.is_empty());
    assert_eq!(file.blocks, vec![20, 40, 21, 22, 30, 31]);

    let displaced = file.replace_block_range(2..5, &[]).unwrap();
    assert_eq!(displaced, vec![21, 22, 30]);
    assert_eq!(file.blocks, vec![20, 40, 31]);

    let displaced = file.replace_block_range(1..3, &[50, 51, 52, 53]).unwrap();
    assert_eq!(displaced, vec![40, 31]);
    assert_eq!(file.blocks, vec![20, 50, 51, 52, 53]);

    store_inode_table(&mut device, &superblock, &[root.clone(), file.clone()]).unwrap();
    assert_eq!(
        load_inode_table(&mut device, &superblock).unwrap(),
        vec![root, file]
    );
}

#[test]
fn invalid_block_count_mutation_is_atomic_before_publication() {
    let mut inode = PersistedInode::new(7, InodeKind::File, vec![11, 13, 17]).unwrap();
    let original = inode.clone();

    let duplicate = inode.replace_block_range(1..1, &[11]).unwrap_err();
    assert_eq!(duplicate.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(inode, original);

    let outside = inode.replace_block_range(4..4, &[19]).unwrap_err();
    assert_eq!(outside.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(inode, original);

    let reversed_start = inode.blocks.len() - 1;
    let reversed = inode
        .replace_block_range(reversed_start..1, &[19])
        .unwrap_err();
    assert_eq!(reversed.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(inode, original);
}
