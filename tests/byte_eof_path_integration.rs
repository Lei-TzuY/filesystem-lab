use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::path_create::create_file_with_blocks_at_path_journaled;
use filesystem_lab::path_file_read::read_file_blocks_at_path;
use filesystem_lab::path_lookup::{
    read_file_range_at_path, truncate_file_at_path_to_bytes_journaled,
    write_file_range_at_path_journaled,
};
use filesystem_lab::path_metadata::metadata_at_path;

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
        u64::try_from(self.blocks.len()).expect("test device block count fits u64")
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

fn setup() -> (MemoryDevice, Superblock) {
    let mut device = MemoryDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, 8).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
            byte_len: 0,
        }],
    )
    .unwrap();

    create_file_with_blocks_at_path_journaled(
        &mut device,
        &superblock,
        "/file",
        &[[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE]],
    )
    .unwrap();
    (device, superblock)
}

#[test]
fn partial_eof_is_observable_and_bounds_pathname_reads_and_writes() {
    let (mut device, superblock) = setup();

    let (released, report) =
        truncate_file_at_path_to_bytes_journaled(&mut device, &superblock, "/file", 5000).unwrap();
    assert!(released.is_empty());
    assert_eq!(report.committed_transactions, 1);

    let metadata = metadata_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(metadata.logical_blocks, 2);
    assert_eq!(metadata.byte_len, 5000);

    let data = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(data.len(), 5000);
    assert!(data[..BLOCK_SIZE].iter().all(|byte| *byte == 0x11));
    assert!(data[BLOCK_SIZE..].iter().all(|byte| *byte == 0x22));

    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/file", 1, 900, 4).unwrap(),
        vec![0x22; 4]
    );
    assert_eq!(
        read_file_range_at_path(&mut device, &superblock, "/file", 1, 900, 5)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );

    write_file_range_at_path_journaled(&mut device, &superblock, "/file", 1, 900, b"tail").unwrap();
    let after_valid_write = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
    assert_eq!(&after_valid_write[4996..5000], b"tail");

    assert_eq!(
        write_file_range_at_path_journaled(&mut device, &superblock, "/file", 1, 900, b"tails",)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap(),
        after_valid_write
    );
    check_device(&mut device).unwrap();
}
