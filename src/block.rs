use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const BLOCK_SIZE: usize = 4096;
pub const BLOCK_SIZE_U64: u64 = BLOCK_SIZE as u64;

pub trait BlockDevice {
    fn block_count(&self) -> u64;
    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()>;
    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

#[derive(Debug)]
pub struct FileBlockDevice {
    file: File,
    blocks: u64,
}

impl FileBlockDevice {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Self::from_file(file)
    }

    pub fn create(path: impl AsRef<Path>, blocks: u64) -> io::Result<Self> {
        let len = blocks.checked_mul(BLOCK_SIZE_U64).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "block device size overflows u64",
            )
        })?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .read(true)
            .write(true)
            .open(path)?;
        file.set_len(len)?;
        Self::from_file(file)
    }

    pub fn from_file(file: File) -> io::Result<Self> {
        let len = file.metadata()?.len();
        if len % BLOCK_SIZE_U64 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "backing file length is not block aligned",
            ));
        }
        Ok(Self {
            file,
            blocks: len / BLOCK_SIZE_U64,
        })
    }

    fn block_offset(&self, block: u64) -> io::Result<u64> {
        if block >= self.blocks {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "block index is outside the device",
            ));
        }
        block.checked_mul(BLOCK_SIZE_U64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "block offset overflows u64")
        })
    }
}

impl BlockDevice for FileBlockDevice {
    fn block_count(&self) -> u64 {
        self.blocks
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let offset = self.block_offset(block)?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let offset = self.block_offset(block)?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.sync_data()
    }
}
