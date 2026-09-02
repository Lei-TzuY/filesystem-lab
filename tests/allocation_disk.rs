use std::collections::BTreeMap;
use std::io;

use filesystem_lab::allocation::BlockAllocator;
use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::{format_device, Superblock};

#[derive(Debug)]
struct SparseBlockDevice {
    block_count: u64,
    blocks: BTreeMap<u64, [u8; BLOCK_SIZE]>,
    flushes: usize,
    fail_writes_to: Option<u64>,
}

impl SparseBlockDevice {
    fn new(block_count: u64) -> Self {
        Self {
            block_count,
            blocks: BTreeMap::new(),
            flushes: 0,
            fail_writes_to: None,
        }
    }
}

impl BlockDevice for SparseBlockDevice {
    fn block_count(&self) -> u64 {
        self.block_count
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        if block >= self.block_count {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "block out of range",
            ));
        }
        if let Some(source) = self.blocks.get(&block) {
            buf.copy_from_slice(source);
        } else {
            buf.fill(0);
        }
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        if block >= self.block_count {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "block out of range",
            ));
        }
        if self.fail_writes_to == Some(block) {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "injected allocation metadata write failure",
            ));
        }
        self.blocks.insert(block, *buf);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

#[test]
fn fresh_format_loads_empty_allocator_with_reserved_prefix() {
    let mut device = SparseBlockDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let allocator = load_allocator(&mut device, &superblock).unwrap();

    assert_eq!(allocator.total_blocks(), 64);
    assert_eq!(allocator.reserved_blocks(), superblock.reserved_blocks());
    assert_eq!(allocator.allocated_blocks(), 0);
    assert!(allocator.is_owned(0).unwrap());
    assert!(!allocator.is_owned(superblock.reserved_blocks()).unwrap());
    allocator.validate().unwrap();
}

#[test]
fn sparse_allocation_round_trips_exact_ownership() {
    let mut device = SparseBlockDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = BlockAllocator::new(64, superblock.reserved_blocks()).unwrap();

    let first = allocator.allocate().unwrap();
    let middle = allocator.allocate().unwrap();
    let last = allocator.allocate().unwrap();
    allocator.free(middle).unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();

    let loaded = load_allocator(&mut device, &superblock).unwrap();
    assert!(loaded.is_owned(first).unwrap());
    assert!(!loaded.is_owned(middle).unwrap());
    assert!(loaded.is_owned(last).unwrap());
    assert_eq!(loaded.allocated_blocks(), 2);
    loaded.validate().unwrap();
}

#[test]
fn checksum_detects_allocation_bitmap_corruption() {
    let mut device = SparseBlockDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let allocation_block = superblock.allocation_start;
    device.blocks.get_mut(&allocation_block).unwrap()[32] ^= 0x10;

    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn geometry_mismatch_is_rejected_before_io() {
    let mut device = SparseBlockDevice::new(63);
    let superblock = Superblock::new(64).unwrap();

    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
}

#[test]
fn failed_header_write_after_tail_update_is_detected_as_torn_image() {
    let mut device = SparseBlockDevice::new(33_000);
    let superblock = format_device(&mut device).unwrap();
    assert_eq!(superblock.allocation_blocks, 2);

    let mut allocator = BlockAllocator::new(
        superblock.total_blocks,
        superblock.reserved_blocks(),
    )
    .unwrap();
    for _ in 0..32_600 {
        allocator.allocate().unwrap();
    }

    device.fail_writes_to = Some(superblock.allocation_start);
    assert_eq!(
        store_allocator(&mut device, &superblock, &allocator)
            .unwrap_err()
            .kind(),
        io::ErrorKind::Other
    );
    device.fail_writes_to = None;

    assert_eq!(
        load_allocator(&mut device, &superblock).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}
