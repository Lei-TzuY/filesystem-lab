use filesystem_lab::block::{BlockDevice, FileBlockDevice, BLOCK_SIZE};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn temp_path(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "filesystem-lab-{name}-{}-{nonce}.img",
        std::process::id()
    ))
}

#[test]
fn create_write_flush_reopen_and_read_round_trip() -> io::Result<()> {
    let path = temp_path("roundtrip");
    let mut dev = FileBlockDevice::create(&path, 4)?;
    assert_eq!(dev.block_count(), 4);

    let mut written = [0_u8; BLOCK_SIZE];
    for (index, byte) in written.iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    dev.write_block(2, &written)?;
    dev.flush()?;
    drop(dev);

    let mut reopened = FileBlockDevice::open(&path)?;
    let mut read = [0_u8; BLOCK_SIZE];
    reopened.read_block(2, &mut read)?;
    assert_eq!(read, written);

    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn rejects_out_of_range_reads_and_writes() -> io::Result<()> {
    let path = temp_path("bounds");
    let mut dev = FileBlockDevice::create(&path, 1)?;
    let mut read = [0_u8; BLOCK_SIZE];
    let write = [0_u8; BLOCK_SIZE];

    assert_eq!(
        dev.read_block(1, &mut read).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert_eq!(
        dev.write_block(1, &write).unwrap_err().kind(),
        io::ErrorKind::UnexpectedEof
    );

    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn rejects_misaligned_backing_files() -> io::Result<()> {
    let path = temp_path("misaligned");
    let mut file = File::create(&path)?;
    file.write_all(&vec![0_u8; BLOCK_SIZE + 1])?;
    drop(file);

    assert_eq!(
        FileBlockDevice::open(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );

    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn rejects_device_size_overflow_before_creating_file() {
    let path = temp_path("overflow");
    let blocks = u64::MAX / BLOCK_SIZE as u64 + 1;
    let error = FileBlockDevice::create(&path, blocks).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(!path.exists());
}
