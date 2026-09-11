use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::path_create::{
    create_empty_file_at_path_journaled, create_file_with_blocks_at_path_journaled,
};
use filesystem_lab::path_file_read::read_file_blocks_at_path;
use filesystem_lab::path_file_write::write_file_blocks_at_path_journaled;
use filesystem_lab::path_symlink::create_symlink_at_path_journaled;

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
        *buf = self.blocks[self.block_index(block)?];
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

fn initialize_root(device: &mut MemoryDevice, superblock: &Superblock) {
    store_inode_table(
        device,
        superblock,
        &[PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        }],
    )
    .unwrap();
}

#[test]
fn overwrites_complete_logical_blocks_and_follows_final_symlink() {
    let mut device = MemoryDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, 8).unwrap();
    initialize_root(&mut device, &superblock);
    create_file_with_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/file",
        &[[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE]],
    )
    .unwrap();
    create_symlink_at_path_journaled(&mut device, &superblock, "/alias", "/file").unwrap();

    let mut replacement = vec![0x33; 2 * BLOCK_SIZE];
    replacement[..4].copy_from_slice(b"head");
    replacement[2 * BLOCK_SIZE - 4..].copy_from_slice(b"tail");
    write_file_blocks_at_path_journaled(&mut device, &superblock, "/alias", &replacement).unwrap();

    assert_eq!(
        read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap(),
        replacement
    );
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_length_mismatch_without_mutating_file() {
    let mut device = MemoryDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, 8).unwrap();
    initialize_root(&mut device, &superblock);
    create_file_with_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/file",
        &[[0x44; BLOCK_SIZE], [0x55; BLOCK_SIZE]],
    )
    .unwrap();
    let before = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();

    assert_eq!(
        write_file_blocks_at_path_journaled(
            &mut device,
            &superblock,
            "/file",
            &vec![0x66; BLOCK_SIZE]
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap(),
        before
    );
    check_device(&mut device).unwrap();
}

#[test]
fn zero_block_file_accepts_only_empty_data_and_non_file_is_rejected() {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device_with_journal_blocks(&mut device, 6).unwrap();
    initialize_root(&mut device, &superblock);
    create_empty_file_at_path_journaled(&mut device, &superblock, "/empty").unwrap();

    write_file_blocks_at_path_journaled(&mut device, &superblock, "/empty", &[]).unwrap();
    assert_eq!(
        write_file_blocks_at_path_journaled(&mut device, &superblock, "/empty", &[0x77])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        write_file_blocks_at_path_journaled(&mut device, &superblock, "/", &[])
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    check_device(&mut device).unwrap();
}
