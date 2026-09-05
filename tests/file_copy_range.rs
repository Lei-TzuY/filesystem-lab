use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_copy_range::copy_file_range_journaled;
use filesystem_lab::file_range_read::read_file_range;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;

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

    fn index(block: u64) -> io::Result<usize> {
        usize::try_from(block).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "block index does not fit host usize",
            )
        })
    }
}

impl BlockDevice for MemoryDevice {
    fn block_count(&self) -> u64 {
        u64::try_from(self.blocks.len()).expect("test device length fits u64")
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = Self::index(block)?;
        let source = self.blocks.get(index).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "block is outside device")
        })?;
        *buf = *source;
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = Self::index(block)?;
        let target = self.blocks.get_mut(index).ok_or_else(|| {
            io::Error::new(io::ErrorKind::UnexpectedEof, "block is outside device")
        })?;
        *target = *buf;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn setup() -> (MemoryDevice, Superblock) {
    let mut device = MemoryDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let source_a = allocator.allocate().unwrap();
    let source_b = allocator.allocate().unwrap();
    let destination_a = allocator.allocate().unwrap();
    let destination_b = allocator.allocate().unwrap();
    let inodes = vec![
        PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        },
        PersistedInode {
            id: 2,
            kind: InodeKind::File,
            blocks: vec![source_a, source_b],
        },
        PersistedInode {
            id: 3,
            kind: InodeKind::File,
            blocks: vec![destination_a, destination_b],
        },
    ];
    let entries = vec![
        PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "source".to_owned(),
        },
        PersistedDirectoryEntry {
            parent: 1,
            target: 3,
            name: "destination".to_owned(),
        },
    ];
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(&mut device, &superblock, &inodes).unwrap();
    store_directory_table(&mut device, &superblock, &entries).unwrap();
    device.write_block(source_a, &[0x11; BLOCK_SIZE]).unwrap();
    device.write_block(source_b, &[0x22; BLOCK_SIZE]).unwrap();
    device.write_block(destination_a, &[0x33; BLOCK_SIZE]).unwrap();
    device.write_block(destination_b, &[0x44; BLOCK_SIZE]).unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn copies_cross_block_range_atomically_into_existing_destination_blocks() {
    let (mut device, superblock) = setup();
    copy_file_range_journaled(
        &mut device,
        &superblock,
        2,
        0,
        BLOCK_SIZE - 3,
        3,
        0,
        BLOCK_SIZE - 2,
        6,
    )
    .unwrap();
    assert_eq!(
        read_file_range(&mut device, &superblock, 3, 0, BLOCK_SIZE - 2, 6).unwrap(),
        vec![0x11, 0x11, 0x11, 0x22, 0x22, 0x22]
    );
    assert_eq!(
        read_file_range(&mut device, &superblock, 2, 0, BLOCK_SIZE - 3, 6).unwrap(),
        vec![0x11, 0x11, 0x11, 0x22, 0x22, 0x22]
    );
    check_device(&mut device).unwrap();
}

#[test]
fn same_inode_overlap_uses_source_snapshot_semantics() {
    let (mut device, superblock) = setup();
    let before = read_file_range(&mut device, &superblock, 2, 0, BLOCK_SIZE - 4, 8).unwrap();
    copy_file_range_journaled(
        &mut device,
        &superblock,
        2,
        0,
        BLOCK_SIZE - 4,
        2,
        0,
        BLOCK_SIZE - 2,
        8,
    )
    .unwrap();
    assert_eq!(
        read_file_range(&mut device, &superblock, 2, 0, BLOCK_SIZE - 2, 8).unwrap(),
        before
    );
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_source_or_destination_ranges_beyond_existing_blocks() {
    let (mut device, superblock) = setup();
    assert_eq!(
        copy_file_range_journaled(
            &mut device,
            &superblock,
            2,
            1,
            BLOCK_SIZE - 1,
            3,
            0,
            0,
            2,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        copy_file_range_journaled(
            &mut device,
            &superblock,
            2,
            0,
            0,
            3,
            1,
            BLOCK_SIZE - 1,
            2,
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::InvalidInput
    );
    check_device(&mut device).unwrap();
}
