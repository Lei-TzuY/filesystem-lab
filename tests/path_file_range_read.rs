use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_data::append_file_block_journaled;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::path_lookup::read_file_range_at_path;
use filesystem_lab::symlink::create_symlink_journaled;

const JOURNAL_BLOCKS: u64 = 6;

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

fn inode(id: u64, kind: InodeKind) -> PersistedInode {
    PersistedInode {
        id,
        kind,
        blocks: Vec::new(),
    }
}

fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry {
        parent,
        target,
        name: name.to_owned(),
    }
}

fn setup() -> (MemoryDevice, Superblock) {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "dir"), entry(2, 3, "file")],
    )
    .unwrap();
    let mut data = [0_u8; BLOCK_SIZE];
    data[100..106].copy_from_slice(b"abcdef");
    append_file_block_journaled(&mut device, &superblock, 3, data).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn reads_regular_file_ranges_through_direct_and_symlink_paths() {
    let (mut device, superblock) = setup();
    create_symlink_journaled(&mut device, &superblock, 1, "dir_alias", "/dir").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "file_alias", "/dir/file").unwrap();

    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 0, 100, 6).unwrap(),
        b"abcdef"
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir_alias/file", 0, 101, 4).unwrap(),
        b"bcde"
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/file_alias", 0, 102, 3).unwrap(),
        b"cde"
    );
    check_device(&mut device).unwrap();
}

#[test]
fn propagates_path_and_regular_file_range_validation() {
    let (mut device, superblock) = setup();
    create_symlink_journaled(&mut device, &superblock, 1, "dangling", "/missing").unwrap();

    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir", 0, 0, 1)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dangling", 0, 0, 1)
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/dir/file", 1, 0, 1)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
}
