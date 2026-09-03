use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};

#[derive(Debug, Clone)]
pub struct CrashDevice {
    durable: Vec<[u8; BLOCK_SIZE]>,
    volatile: Vec<[u8; BLOCK_SIZE]>,
    crash_at: Option<usize>,
    operations: usize,
    armed: bool,
}

impl CrashDevice {
    pub fn new(blocks: usize) -> Self {
        let durable = vec![[0_u8; BLOCK_SIZE]; blocks];
        Self {
            volatile: durable.clone(),
            durable,
            crash_at: None,
            operations: 0,
            armed: false,
        }
    }

    pub fn arm(&mut self, crash_at: Option<usize>) {
        self.crash_at = crash_at;
        self.operations = 0;
        self.armed = true;
    }

    pub fn operations(&self) -> usize {
        self.operations
    }

    pub fn reboot(&mut self) {
        self.volatile.clone_from(&self.durable);
        self.crash_at = None;
        self.operations = 0;
        self.armed = false;
    }

    fn before_mutation(&mut self) -> io::Result<()> {
        if self.armed && self.crash_at == Some(self.operations) {
            return Err(io::Error::other(format!(
                "deterministic crash at mutation operation {}",
                self.operations
            )));
        }
        if self.armed {
            self.operations += 1;
        }
        Ok(())
    }
}

impl BlockDevice for CrashDevice {
    fn block_count(&self) -> u64 {
        u64::try_from(self.volatile.len()).expect("test device length fits u64")
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
        *buf = *self
            .volatile
            .get(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        self.before_mutation()?;
        let index = usize::try_from(block)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
        *self
            .volatile
            .get_mut(index)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))? = *buf;
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.before_mutation()?;
        self.durable.clone_from(&self.volatile);
        Ok(())
    }
}
