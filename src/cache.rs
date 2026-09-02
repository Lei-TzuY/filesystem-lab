use std::collections::BTreeMap;
use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferState {
    Clean,
    Dirty,
    Writeback,
}

#[derive(Debug)]
struct CacheEntry {
    data: [u8; BLOCK_SIZE],
    state: BufferState,
}

#[derive(Debug)]
pub struct BufferCache<D> {
    device: D,
    entries: BTreeMap<u64, CacheEntry>,
}

impl<D: BlockDevice> BufferCache<D> {
    #[must_use]
    pub fn new(device: D) -> Self {
        Self {
            device,
            entries: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn device(&self) -> &D {
        &self.device
    }

    #[must_use]
    pub fn cached_blocks(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn state(&self, block: u64) -> Option<BufferState> {
        self.entries.get(&block).map(|entry| entry.state)
    }

    /// Reads a block through the cache.
    ///
    /// A cache miss reads from the underlying block device and installs a clean entry.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if `block` is outside the device or the underlying read fails.
    pub fn read_block(&mut self, block: u64) -> io::Result<[u8; BLOCK_SIZE]> {
        self.validate_block(block)?;
        if !self.entries.contains_key(&block) {
            let mut data = [0_u8; BLOCK_SIZE];
            self.device.read_block(block, &mut data)?;
            self.entries.insert(
                block,
                CacheEntry {
                    data,
                    state: BufferState::Clean,
                },
            );
        }

        self.entries
            .get(&block)
            .map(|entry| entry.data)
            .ok_or_else(|| io::Error::other("cache entry disappeared after insertion"))
    }

    /// Replaces a cached block and marks it dirty without issuing device I/O.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if `block` is outside the underlying device.
    pub fn write_block(&mut self, block: u64, data: [u8; BLOCK_SIZE]) -> io::Result<()> {
        self.validate_block(block)?;
        self.entries.insert(
            block,
            CacheEntry {
                data,
                state: BufferState::Dirty,
            },
        );
        Ok(())
    }

    /// Issues a write for one dirty block without crossing the durability boundary.
    ///
    /// A successful write transitions `Dirty -> Writeback`. Clean and already-writeback entries are
    /// left unchanged. The entry does not become clean until [`Self::flush`] successfully completes.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the block is not cached or the underlying write fails.
    pub fn write_back(&mut self, block: u64) -> io::Result<()> {
        let Some(entry) = self.entries.get(&block) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cannot write back an uncached block",
            ));
        };

        if entry.state != BufferState::Dirty {
            return Ok(());
        }

        let data = entry.data;
        self.device.write_block(block, &data)?;
        if let Some(entry) = self.entries.get_mut(&block) {
            entry.state = BufferState::Writeback;
        }
        Ok(())
    }

    /// Writes every dirty entry, flushes the device, and marks durable entries clean.
    ///
    /// Dirty entries first transition to `Writeback` as writes are issued. Only a successful device
    /// flush transitions all writeback entries to `Clean`. If the device flush fails, writeback
    /// entries remain writeback so a later retry can re-attempt only the durability boundary.
    ///
    /// # Errors
    ///
    /// Returns the first underlying write or flush error. Entries whose writes completed before an
    /// error retain the `Writeback` state.
    pub fn flush(&mut self) -> io::Result<()> {
        let dirty_blocks: Vec<u64> = self
            .entries
            .iter()
            .filter_map(|(&block, entry)| (entry.state == BufferState::Dirty).then_some(block))
            .collect();

        for block in dirty_blocks {
            self.write_back(block)?;
        }

        if self
            .entries
            .values()
            .any(|entry| entry.state == BufferState::Writeback)
        {
            self.device.flush()?;
            for entry in self.entries.values_mut() {
                if entry.state == BufferState::Writeback {
                    entry.state = BufferState::Clean;
                }
            }
        }

        Ok(())
    }

    /// Evicts one clean block from the cache.
    ///
    /// Dirty and writeback entries cannot be evicted because doing so would discard data that is not
    /// known to be durable.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the block is not cached or is not clean.
    pub fn evict(&mut self, block: u64) -> io::Result<()> {
        let Some(entry) = self.entries.get(&block) else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "cannot evict an uncached block",
            ));
        };
        if entry.state != BufferState::Clean {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "cannot evict a block that is not durable",
            ));
        }
        self.entries.remove(&block);
        Ok(())
    }

    /// Checks cache invariants against the underlying block device.
    ///
    /// # Errors
    ///
    /// Returns an error if a cached key lies outside the device's logical block range.
    pub fn validate_invariants(&self) -> io::Result<()> {
        if let Some((&block, _)) = self
            .entries
            .iter()
            .find(|(block, _)| **block >= self.device.block_count())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("cached block {block} is outside the device"),
            ));
        }
        Ok(())
    }

    #[must_use]
    pub fn into_inner(self) -> D {
        self.device
    }

    fn validate_block(&self, block: u64) -> io::Result<()> {
        if block >= self.device.block_count() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "block index is outside the device",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Debug)]
    struct MockDevice {
        blocks: u64,
        durable: BTreeMap<u64, [u8; BLOCK_SIZE]>,
        pending: BTreeMap<u64, [u8; BLOCK_SIZE]>,
        writes: usize,
        flushes: usize,
        fail_next_flush: bool,
    }

    impl MockDevice {
        fn new(blocks: u64) -> Self {
            Self {
                blocks,
                durable: BTreeMap::new(),
                pending: BTreeMap::new(),
                writes: 0,
                flushes: 0,
                fail_next_flush: false,
            }
        }
    }

    impl BlockDevice for MockDevice {
        fn block_count(&self) -> u64 {
            self.blocks
        }

        fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
            if block >= self.blocks {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "out of range"));
            }
            *buf = self
                .durable
                .get(&block)
                .copied()
                .unwrap_or([0_u8; BLOCK_SIZE]);
            Ok(())
        }

        fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
            if block >= self.blocks {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "out of range"));
            }
            self.pending.insert(block, *buf);
            self.writes += 1;
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            self.flushes += 1;
            if self.fail_next_flush {
                self.fail_next_flush = false;
                return Err(io::Error::other("injected flush failure"));
            }
            self.durable.append(&mut self.pending);
            Ok(())
        }
    }

    #[test]
    fn write_is_dirty_without_device_io() {
        let device = MockDevice::new(8);
        let mut cache = BufferCache::new(device);
        let data = [0xA5; BLOCK_SIZE];

        cache.write_block(3, data).unwrap();

        assert_eq!(cache.state(3), Some(BufferState::Dirty));
        assert_eq!(cache.device().writes, 0);
        assert_eq!(cache.device().flushes, 0);
        assert!(!cache.device().durable.contains_key(&3));
    }

    #[test]
    fn writeback_is_not_durability() {
        let device = MockDevice::new(8);
        let mut cache = BufferCache::new(device);
        let data = [0x5A; BLOCK_SIZE];
        cache.write_block(2, data).unwrap();

        cache.write_back(2).unwrap();

        assert_eq!(cache.state(2), Some(BufferState::Writeback));
        assert_eq!(cache.device().writes, 1);
        assert_eq!(cache.device().flushes, 0);
        assert_eq!(cache.device().pending.get(&2), Some(&data));
        assert!(!cache.device().durable.contains_key(&2));
        assert_eq!(
            cache.evict(2).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn flush_makes_written_blocks_clean_and_evictable() {
        let device = MockDevice::new(8);
        let mut cache = BufferCache::new(device);
        let data = [7; BLOCK_SIZE];
        cache.write_block(4, data).unwrap();

        cache.flush().unwrap();

        assert_eq!(cache.state(4), Some(BufferState::Clean));
        assert_eq!(cache.device().writes, 1);
        assert_eq!(cache.device().flushes, 1);
        assert_eq!(cache.device().durable.get(&4), Some(&data));
        cache.evict(4).unwrap();
        assert_eq!(cache.state(4), None);
    }

    #[test]
    fn failed_flush_retains_writeback_state_and_retry_does_not_rewrite() {
        let mut device = MockDevice::new(8);
        device.fail_next_flush = true;
        let mut cache = BufferCache::new(device);
        let data = [9; BLOCK_SIZE];
        cache.write_block(5, data).unwrap();

        assert_eq!(cache.flush().unwrap_err().kind(), io::ErrorKind::Other);
        assert_eq!(cache.state(5), Some(BufferState::Writeback));
        assert_eq!(cache.device().writes, 1);
        assert_eq!(cache.device().flushes, 1);

        cache.flush().unwrap();

        assert_eq!(cache.state(5), Some(BufferState::Clean));
        assert_eq!(cache.device().writes, 1);
        assert_eq!(cache.device().flushes, 2);
        assert_eq!(cache.device().durable.get(&5), Some(&data));
    }

    #[test]
    fn read_miss_installs_clean_entry() {
        let mut device = MockDevice::new(4);
        let data = [3; BLOCK_SIZE];
        device.durable.insert(1, data);
        let mut cache = BufferCache::new(device);

        assert_eq!(cache.read_block(1).unwrap(), data);
        assert_eq!(cache.state(1), Some(BufferState::Clean));
        assert_eq!(cache.cached_blocks(), 1);
        cache.validate_invariants().unwrap();
    }

    #[test]
    fn out_of_range_operations_are_rejected() {
        let device = MockDevice::new(2);
        let mut cache = BufferCache::new(device);

        assert_eq!(
            cache.write_block(2, [0; BLOCK_SIZE]).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
        assert_eq!(
            cache.read_block(2).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
}
