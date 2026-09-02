use std::io;

use crate::block::BLOCK_SIZE;
use crate::journal::{JournalEntry, TransactionId};

const RECORD_MAGIC: [u8; 4] = *b"JNL1";
const RECORD_VERSION: u16 = 1;
const HEADER_SIZE: usize = 32;
const CHECKSUM_OFFSET: usize = 28;
const KIND_BEGIN: u8 = 1;
const KIND_WRITE: u8 = 2;
const KIND_COMMIT: u8 = 3;

/// Encodes journal entries into deterministic persistent records.
///
/// Each record has a fixed 32-byte little-endian header containing magic, codec version, record
/// kind, flags, total record length, transaction identifier, target block, and an IEEE CRC-32.
/// Write records append exactly one logical block of payload. The checksum covers the entire record
/// with the checksum field itself treated as zero.
///
/// # Errors
///
/// Returns an error if a record length cannot be represented by the on-disk `u32` length field.
pub fn encode_entries(entries: &[JournalEntry]) -> io::Result<Vec<u8>> {
    let mut encoded = Vec::new();
    for entry in entries {
        let (kind, txid, block, payload): (u8, TransactionId, u64, &[u8]) = match entry {
            JournalEntry::Begin { txid } => (KIND_BEGIN, *txid, 0, &[]),
            JournalEntry::Write { txid, block, data } => {
                (KIND_WRITE, *txid, *block, data.as_slice())
            }
            JournalEntry::Commit { txid } => (KIND_COMMIT, *txid, 0, &[]),
        };

        let record_len = HEADER_SIZE
            .checked_add(payload.len())
            .ok_or_else(|| invalid_input("journal record length overflow"))?;
        let record_len_u32 = u32::try_from(record_len)
            .map_err(|_| invalid_input("journal record length exceeds u32"))?;

        let start = encoded.len();
        encoded.extend_from_slice(&RECORD_MAGIC);
        encoded.extend_from_slice(&RECORD_VERSION.to_le_bytes());
        encoded.push(kind);
        encoded.push(0); // flags
        encoded.extend_from_slice(&record_len_u32.to_le_bytes());
        encoded.extend_from_slice(&txid.to_le_bytes());
        encoded.extend_from_slice(&block.to_le_bytes());
        encoded.extend_from_slice(&0_u32.to_le_bytes());
        encoded.extend_from_slice(payload);

        let checksum = crc32(&encoded[start..]);
        encoded[start + CHECKSUM_OFFSET..start + CHECKSUM_OFFSET + 4]
            .copy_from_slice(&checksum.to_le_bytes());
    }
    Ok(encoded)
}

/// Decodes persistent journal records while rejecting torn, corrupt, or unsupported input.
///
/// # Errors
///
/// Returns `InvalidData` for bad magic/version/kind/flags, impossible record lengths, truncated
/// records, checksum mismatch, non-zero metadata in begin/commit records, or malformed write sizes.
pub fn decode_entries(bytes: &[u8]) -> io::Result<Vec<JournalEntry>> {
    let mut entries = Vec::new();
    let mut cursor = 0_usize;

    while cursor < bytes.len() {
        let remaining = bytes.len() - cursor;
        if remaining < HEADER_SIZE {
            return Err(invalid_data("truncated journal record header"));
        }

        let header = &bytes[cursor..cursor + HEADER_SIZE];
        if header[0..4] != RECORD_MAGIC {
            return Err(invalid_data("invalid journal record magic"));
        }

        let version = u16::from_le_bytes([header[4], header[5]]);
        if version != RECORD_VERSION {
            return Err(invalid_data("unsupported journal record version"));
        }
        let kind = header[6];
        if header[7] != 0 {
            return Err(invalid_data("unsupported journal record flags"));
        }

        let record_len = usize::try_from(u32::from_le_bytes([
            header[8], header[9], header[10], header[11],
        ]))
        .map_err(|_| invalid_data("journal record length does not fit usize"))?;
        if record_len < HEADER_SIZE {
            return Err(invalid_data("journal record length is smaller than header"));
        }
        let end = cursor
            .checked_add(record_len)
            .ok_or_else(|| invalid_data("journal record range overflow"))?;
        if end > bytes.len() {
            return Err(invalid_data("truncated journal record payload"));
        }

        let txid = u64::from_le_bytes(
            header[12..20]
                .try_into()
                .map_err(|_| invalid_data("journal transaction identifier field is malformed"))?,
        );
        let block = u64::from_le_bytes(
            header[20..28]
                .try_into()
                .map_err(|_| invalid_data("journal block field is malformed"))?,
        );
        let expected_checksum = u32::from_le_bytes(
            header[28..32]
                .try_into()
                .map_err(|_| invalid_data("journal checksum field is malformed"))?,
        );

        let mut record = bytes[cursor..end].to_vec();
        record[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].fill(0);
        if crc32(&record) != expected_checksum {
            return Err(invalid_data("journal record checksum mismatch"));
        }

        let payload = &bytes[cursor + HEADER_SIZE..end];
        let entry = match kind {
            KIND_BEGIN => {
                require_control_record(block, payload)?;
                JournalEntry::Begin { txid }
            }
            KIND_WRITE => {
                if payload.len() != BLOCK_SIZE {
                    return Err(invalid_data(
                        "journal write record has invalid payload length",
                    ));
                }
                let mut data = Box::new([0_u8; BLOCK_SIZE]);
                data.copy_from_slice(payload);
                JournalEntry::Write { txid, block, data }
            }
            KIND_COMMIT => {
                require_control_record(block, payload)?;
                JournalEntry::Commit { txid }
            }
            _ => return Err(invalid_data("unknown journal record kind")),
        };
        entries.push(entry);
        cursor = end;
    }

    Ok(entries)
}

fn require_control_record(block: u64, payload: &[u8]) -> io::Result<()> {
    if block != 0 {
        return Err(invalid_data(
            "journal control record has non-zero block field",
        ));
    }
    if !payload.is_empty() {
        return Err(invalid_data(
            "journal control record has unexpected payload",
        ));
    }
    Ok(())
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::JournalLog;

    fn sample_entries() -> Vec<JournalEntry> {
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, 9, [0x5a; BLOCK_SIZE]).unwrap();
        log.commit(txid).unwrap();
        log.entries().to_vec()
    }

    #[test]
    fn round_trip_is_deterministic() {
        let entries = sample_entries();
        let first = encode_entries(&entries).unwrap();
        let second = encode_entries(&entries).unwrap();
        assert_eq!(first, second);
        assert_eq!(decode_entries(&first).unwrap(), entries);
    }

    #[test]
    fn truncated_header_is_rejected() {
        let encoded = encode_entries(&sample_entries()).unwrap();
        assert_eq!(
            decode_entries(&encoded[..HEADER_SIZE - 1])
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn torn_write_payload_is_rejected() {
        let entries = sample_entries();
        let encoded = encode_entries(&entries).unwrap();
        let begin_len = HEADER_SIZE;
        let torn_end = begin_len + HEADER_SIZE + BLOCK_SIZE - 1;
        assert_eq!(
            decode_entries(&encoded[..torn_end]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn checksum_detects_payload_corruption() {
        let mut encoded = encode_entries(&sample_entries()).unwrap();
        let payload_offset = HEADER_SIZE + HEADER_SIZE;
        encoded[payload_offset] ^= 0xff;
        assert_eq!(
            decode_entries(&encoded).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn checksum_detects_header_corruption() {
        let mut encoded = encode_entries(&sample_entries()).unwrap();
        encoded[12] ^= 1;
        assert_eq!(
            decode_entries(&encoded).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn unsupported_version_is_rejected_before_checksum() {
        let mut encoded = encode_entries(&sample_entries()).unwrap();
        encoded[4..6].copy_from_slice(&2_u16.to_le_bytes());
        assert_eq!(
            decode_entries(&encoded).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn unknown_kind_is_rejected_with_valid_checksum() {
        let mut encoded = encode_entries(&sample_entries()).unwrap();
        encoded[6] = 0xff;
        encoded[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].fill(0);
        let checksum = crc32(&encoded[..HEADER_SIZE]);
        encoded[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
        assert_eq!(
            decode_entries(&encoded).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn non_zero_flags_are_rejected() {
        let mut encoded = encode_entries(&sample_entries()).unwrap();
        encoded[7] = 1;
        assert_eq!(
            decode_entries(&encoded).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn record_length_smaller_than_header_is_rejected() {
        let mut encoded = encode_entries(&sample_entries()).unwrap();
        encoded[8..12].copy_from_slice(&1_u32.to_le_bytes());
        assert_eq!(
            decode_entries(&encoded).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn empty_stream_decodes_to_empty_log() {
        assert!(decode_entries(&[]).unwrap().is_empty());
    }
}
