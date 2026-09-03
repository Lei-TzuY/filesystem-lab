use std::collections::BTreeMap;
use std::io;
use std::ops::Range;

use crate::block::{BlockDevice, BLOCK_SIZE};

/// In-memory block device used to render metadata table images before publishing them through the
/// write-ahead log.
///
/// Keeping this capture primitive in one internal module ensures allocation/inode/directory and
/// cross-table transaction paths share the same bounds and zero-fill semantics instead of carrying
/// slightly different private copies.
#[derive(Debug)]
pub(crate) struct CaptureDevice {
    block_count: u64,
    blocks: BTreeMap<u64, [u8; BLOCK_SIZE]>,
}

impl CaptureDevice {
    pub(crate) fn new(block_count: u64) -> Self {
        Self {
            block_count,
            blocks: BTreeMap::new(),
        }
    }

    pub(crate) fn take_rendered_block(
        &mut self,
        block: u64,
        missing_message: &str,
    ) -> io::Result<[u8; BLOCK_SIZE]> {
        self.blocks
            .remove(&block)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, missing_message))
    }

    pub(crate) fn collect_changed_range(
        &mut self,
        device: &mut impl BlockDevice,
        range: Range<u64>,
        missing_message: &str,
        changed: &mut Vec<(u64, [u8; BLOCK_SIZE])>,
    ) -> io::Result<()> {
        for block in range {
            let desired = self.take_rendered_block(block, missing_message)?;
            let mut current = [0_u8; BLOCK_SIZE];
            device.read_block(block, &mut current)?;
            if current != desired {
                changed.push((block, desired));
            }
        }
        Ok(())
    }

    pub(crate) fn ensure_empty(&self, unexpected_message: &str) -> io::Result<()> {
        if self.blocks.is_empty() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                unexpected_message,
            ))
        }
    }
}

impl BlockDevice for CaptureDevice {
    fn block_count(&self) -> u64 {
        self.block_count
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        if block >= self.block_count {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "capture-device read is out of range",
            ));
        }
        if let Some(data) = self.blocks.get(&block) {
            *buf = *data;
        } else {
            buf.fill(0);
        }
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        if block >= self.block_count {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "capture-device write is out of range",
            ));
        }
        self.blocks.insert(block, *buf);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            self.blocks.len() as u64
        }

        fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
            *buf = self.blocks[block as usize];
            Ok(())
        }

        fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
            self.blocks[block as usize] = *buf;
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn changed_range_consumes_rendered_blocks_and_skips_identical_home_blocks() {
        let mut capture = CaptureDevice::new(4);
        let mut first = [0_u8; BLOCK_SIZE];
        first[0] = 1;
        let mut second = [0_u8; BLOCK_SIZE];
        second[0] = 2;
        capture.write_block(1, &first).unwrap();
        capture.write_block(2, &second).unwrap();

        let mut device = MemoryDevice::new(4);
        device.write_block(1, &first).unwrap();
        let mut changed = Vec::new();
        capture
            .collect_changed_range(
                &mut device,
                1..3,
                "missing rendered metadata block",
                &mut changed,
            )
            .unwrap();

        assert_eq!(changed, vec![(2, second)]);
        capture.ensure_empty("unexpected rendered block").unwrap();
    }

    #[test]
    fn rejects_rendered_blocks_outside_consumed_regions() {
        let mut capture = CaptureDevice::new(4);
        capture.write_block(3, &[0xa5; BLOCK_SIZE]).unwrap();

        let error = capture
            .ensure_empty("rendered outside expected metadata regions")
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
