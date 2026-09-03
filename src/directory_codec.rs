use std::{io, str};

pub const DIRECTORY_ENTRY_MAGIC: [u8; 4] = *b"DNT1";
pub const DIRECTORY_ENTRY_VERSION: u16 = 1;
pub const DIRECTORY_ENTRY_HEADER_LEN: usize = 40;
pub const DIRECTORY_NAME_MAX_BYTES: usize = 255;

const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 4;
const FLAGS_OFFSET: usize = 6;
const TOTAL_LEN_OFFSET: usize = 8;
const NAME_LEN_OFFSET: usize = 12;
const RESERVED16_OFFSET: usize = 14;
const PARENT_ID_OFFSET: usize = 16;
const TARGET_ID_OFFSET: usize = 24;
const CRC_OFFSET: usize = 32;
const RESERVED32_OFFSET: usize = 36;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedDirectoryEntry {
    pub parent: u64,
    pub target: u64,
    pub name: String,
}

/// Encodes one directory entry into a self-delimiting, checksummed little-endian record.
///
/// # Errors
///
/// Returns `InvalidInput` when either inode identifier is zero, the name is not a valid single
/// path component, or the encoded length exceeds the codec limits.
pub fn encode_directory_entry(entry: &PersistedDirectoryEntry) -> io::Result<Vec<u8>> {
    if entry.parent == 0 || entry.target == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory entry inode identifier zero is reserved",
        ));
    }
    validate_name(&entry.name, io::ErrorKind::InvalidInput)?;

    let name_len = u16::try_from(entry.name.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory entry name length exceeds codec limit",
        )
    })?;
    let total_len = DIRECTORY_ENTRY_HEADER_LEN
        .checked_add(entry.name.len())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "directory entry size overflow")
        })?;
    let total_len_u32 = u32::try_from(total_len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory entry length exceeds codec limit",
        )
    })?;

    let mut bytes = vec![0_u8; total_len];
    bytes[MAGIC_OFFSET..MAGIC_OFFSET + 4].copy_from_slice(&DIRECTORY_ENTRY_MAGIC);
    bytes[VERSION_OFFSET..VERSION_OFFSET + 2]
        .copy_from_slice(&DIRECTORY_ENTRY_VERSION.to_le_bytes());
    bytes[TOTAL_LEN_OFFSET..TOTAL_LEN_OFFSET + 4].copy_from_slice(&total_len_u32.to_le_bytes());
    bytes[NAME_LEN_OFFSET..NAME_LEN_OFFSET + 2].copy_from_slice(&name_len.to_le_bytes());
    bytes[PARENT_ID_OFFSET..PARENT_ID_OFFSET + 8].copy_from_slice(&entry.parent.to_le_bytes());
    bytes[TARGET_ID_OFFSET..TARGET_ID_OFFSET + 8].copy_from_slice(&entry.target.to_le_bytes());
    bytes[DIRECTORY_ENTRY_HEADER_LEN..].copy_from_slice(entry.name.as_bytes());

    let crc = record_crc(&bytes);
    bytes[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
    Ok(bytes)
}

/// Decodes and validates exactly one directory entry record.
///
/// # Errors
///
/// Returns `UnexpectedEof` for a torn header or payload and `InvalidData` for bad magic, version,
/// flags, reserved fields, inconsistent lengths, checksum mismatch, zero inode identifiers, invalid
/// UTF-8, or an invalid path-component name.
pub fn decode_directory_entry(bytes: &[u8]) -> io::Result<PersistedDirectoryEntry> {
    if bytes.len() < DIRECTORY_ENTRY_HEADER_LEN {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "torn directory entry header",
        ));
    }
    if bytes[MAGIC_OFFSET..MAGIC_OFFSET + 4] != DIRECTORY_ENTRY_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid directory entry magic",
        ));
    }
    let version = read_u16(bytes, VERSION_OFFSET);
    if version != DIRECTORY_ENTRY_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported directory entry version {version}"),
        ));
    }
    if read_u16(bytes, FLAGS_OFFSET) != 0
        || read_u16(bytes, RESERVED16_OFFSET) != 0
        || read_u32(bytes, RESERVED32_OFFSET) != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry reserved fields are non-zero",
        ));
    }

    let total_len = usize::try_from(read_u32(bytes, TOTAL_LEN_OFFSET)).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry record length is invalid",
        )
    })?;
    let name_len = usize::from(read_u16(bytes, NAME_LEN_OFFSET));
    let expected_len = DIRECTORY_ENTRY_HEADER_LEN
        .checked_add(name_len)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "directory entry size overflow")
        })?;
    if total_len != expected_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry length does not match name length",
        ));
    }
    if bytes.len() < total_len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "torn directory entry payload",
        ));
    }
    if bytes.len() != total_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry decoder requires exactly one record",
        ));
    }

    let stored_crc = read_u32(bytes, CRC_OFFSET);
    if stored_crc != record_crc(bytes) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry checksum mismatch",
        ));
    }

    let parent = read_u64(bytes, PARENT_ID_OFFSET);
    let target = read_u64(bytes, TARGET_ID_OFFSET);
    if parent == 0 || target == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry inode identifier zero is reserved",
        ));
    }

    let name = str::from_utf8(&bytes[DIRECTORY_ENTRY_HEADER_LEN..]).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "directory entry name is not UTF-8",
        )
    })?;
    validate_name(name, io::ErrorKind::InvalidData)?;

    Ok(PersistedDirectoryEntry {
        parent,
        target,
        name: name.to_owned(),
    })
}

fn validate_name(name: &str, kind: io::ErrorKind) -> io::Result<()> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\0')
        || name.len() > DIRECTORY_NAME_MAX_BYTES
    {
        return Err(io::Error::new(kind, "invalid directory entry name"));
    }
    Ok(())
}

fn record_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for (index, byte) in bytes.iter().enumerate() {
        let value = if (CRC_OFFSET..CRC_OFFSET + 4).contains(&index) {
            0
        } else {
            *byte
        };
        crc ^= u32::from(value);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PersistedDirectoryEntry {
        PersistedDirectoryEntry {
            parent: 2,
            target: 7,
            name: "hello.txt".to_owned(),
        }
    }

    #[test]
    fn round_trip_preserves_directory_entry() {
        let entry = sample();
        let encoded = encode_directory_entry(&entry).unwrap();
        assert_eq!(decode_directory_entry(&encoded).unwrap(), entry);
    }

    #[test]
    fn accepts_maximum_length_utf8_name() {
        let entry = PersistedDirectoryEntry {
            parent: 2,
            target: 8,
            name: "a".repeat(DIRECTORY_NAME_MAX_BYTES),
        };
        assert_eq!(
            decode_directory_entry(&encode_directory_entry(&entry).unwrap()).unwrap(),
            entry
        );
    }

    #[test]
    fn rejects_invalid_component_names() {
        for name in ["", ".", "..", "a/b", "a\0b"] {
            let mut entry = sample();
            entry.name = name.to_owned();
            let error = encode_directory_entry(&entry).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn rejects_overlong_name() {
        let mut entry = sample();
        entry.name = "a".repeat(DIRECTORY_NAME_MAX_BYTES + 1);
        let error = encode_directory_entry(&entry).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn detects_torn_payload() {
        let encoded = encode_directory_entry(&sample()).unwrap();
        let error = decode_directory_entry(&encoded[..encoded.len() - 1]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn detects_corruption() {
        let mut encoded = encode_directory_entry(&sample()).unwrap();
        *encoded.last_mut().unwrap() ^= 0x80;
        let error = decode_directory_entry(&encoded).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_nonzero_reserved_fields_even_with_recomputed_crc() {
        let mut encoded = encode_directory_entry(&sample()).unwrap();
        encoded[FLAGS_OFFSET] = 1;
        let crc = record_crc(&encoded);
        encoded[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        let error = decode_directory_entry(&encoded).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_invalid_utf8_even_with_recomputed_crc() {
        let mut encoded = encode_directory_entry(&sample()).unwrap();
        encoded[DIRECTORY_ENTRY_HEADER_LEN] = 0xff;
        let crc = record_crc(&encoded);
        encoded[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        let error = decode_directory_entry(&encoded).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
