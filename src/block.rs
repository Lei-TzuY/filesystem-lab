use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const BLOCK_SIZE: usize = 4096;
pub const BLOCK_SIZE_U64: u64 = BLOCK_SIZE as u64;

pub trait BlockDevice {
    fn block_count(&self) -> u64;

    /// Reads one logical block into `buf`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the requested block is invalid or the underlying device cannot
    /// complete the read.
    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()>;

    /// Writes one logical block from `buf`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the requested block is invalid or the underlying device cannot
    /// complete the write.
    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()>;

    /// Flushes previously issued writes through the device's durability boundary.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the underlying device cannot make prior writes durable.
    fn flush(&mut self) -> io::Result<()>;
}

#[derive(Debug)]
pub struct FileBlockDevice {
    file: File,
    blocks: u64,
}

impl FileBlockDevice {
    /// Opens an existing block-aligned file as a block device.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file cannot be opened, inspected, or has a length that is not a
    /// multiple of [`BLOCK_SIZE`].
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        Self::from_file(file)
    }

    /// Creates or truncates a file-backed block device with `blocks` logical blocks.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the requested device size overflows `u64`, the file cannot be
    /// created or resized, or the resulting file fails block-device validation.
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

    /// Validates and wraps an existing file as a block device.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if file metadata cannot be read or the file length is not aligned to
    /// [`BLOCK_SIZE`].
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
