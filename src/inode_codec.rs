use std::io;
use std::ops::Range;

use crate::inode::{Inode, InodeKind};

pub const INODE_RECORD_MAGIC: [u8; 4] = *b"INO1";
pub const INODE_RECORD_VERSION: u16 = 2;
pub const INODE_RECORD_HEADER_LEN: usize = 32;

const KIND_FILE: u16 = 1;
const KIND_DIRECTORY: u16 = 2;
const KIND_SYMLINK: u16 = 3;
const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 4;
const KIND_OFFSET: usize = 6;
const TOTAL_LEN_OFFSET: usize = 8;
const INODE_ID_OFFSET: usize = 12;
const BLOCK_COUNT_OFFSET: usize = 20;
const CRC_OFFSET: usize = 24;
const RESERVED_OFFSET: usize = 28;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedInode {
    pub id: u64,
    pub kind: InodeKind,
    pub blocks: Vec<u64>,
}

impl PersistedInode {
    /// Constructs one persistence-safe inode value.
    ///
    /// This is the production construction boundary for durable inode records. Keeping creation
    /// behind one invariant gate allows future record revisions to add persisted fields without
    /// duplicating validation policy across pathname and lifecycle code.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the inode identifier is zero or the block vector contains a
    /// duplicate physical block reference.
    pub fn new(id: u64, kind: InodeKind, blocks: Vec<u64>) -> io::Result<Self> {
        validate_inode_fields(id, &blocks)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        Ok(Self { id, kind, blocks })
    }

    /// Validates invariants that must hold before an inode can be encoded durably.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for an invalid identifier or duplicate block references.
    pub fn validate_for_persistence(&self) -> io::Result<()> {
        validate_inode_fields(self.id, &self.blocks)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))
    }

    /// Replaces one logical block range while preserving durable inode invariants.
    ///
    /// The mutation is prepared on a candidate vector and committed to `self.blocks` only after
    /// the complete resulting mapping validates. This makes block-count-changing operations share
    /// one mutation boundary instead of editing the public compatibility vector in place.
    ///
    /// The returned vector contains the displaced blocks in logical order.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the range is reversed or outside the current block vector, or
    /// when the resulting mapping would contain duplicate physical block references.
    pub fn replace_block_range(
        &mut self,
        range: Range<usize>,
        replacements: &[u64],
    ) -> io::Result<Vec<u64>> {
        if range.start > range.end || range.end > self.blocks.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "inode block mutation range is outside the current mapping",
            ));
        }

        let displaced = self.blocks[range.clone()].to_vec();
        let mut candidate = self.blocks.clone();
        candidate.splice(range, replacements.iter().copied());
        validate_inode_fields(self.id, &candidate)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        self.blocks = candidate;
        Ok(displaced)
    }
}

impl From<&Inode> for PersistedInode {
    fn from(inode: &Inode) -> Self {
        Self {
            id: inode.id().get(),
            kind: inode.kind(),
            blocks: inode.blocks().to_vec(),
        }
    }
}

/// Encodes one inode into a self-delimiting, checksummed little-endian record.
///
/// # Errors
///
/// Returns `InvalidInput` when the inode identifier is zero, the block count cannot fit in the
/// record header, the encoded length overflows, or the same block is referenced more than once.
pub fn encode_inode(inode: &PersistedInode) -> io::Result<Vec<u8>> {
    inode.validate_for_persistence()?;
    let block_count = u32::try_from(inode.blocks.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "inode block count exceeds codec limit",
        )
    })?;
    let payload_len =
        inode.blocks.len().checked_mul(8).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "inode record size overflow")
        })?;
    let total_len = INODE_RECORD_HEADER_LEN
        .checked_add(payload_len)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "inode record size overflow"))?;
    let total_len_u32 = u32::try_from(total_len).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "inode record length exceeds codec limit",
        )
    })?;

    let mut bytes = vec![0_u8; total_len];
    bytes[MAGIC_OFFSET..MAGIC_OFFSET + 4].copy_from_slice(&INODE_RECORD_MAGIC);
    bytes[VERSION_OFFSET..VERSION_OFFSET + 2].copy_from_slice(&INODE_RECORD_VERSION.to_le_bytes());
    bytes[KIND_OFFSET..KIND_OFFSET + 2].copy_from_slice(&kind_code(inode.kind).to_le_bytes());
    bytes[TOTAL_LEN_OFFSET..TOTAL_LEN_OFFSET + 4].copy_from_slice(&total_len_u32.to_le_bytes());
    bytes[INODE_ID_OFFSET..INODE_ID_OFFSET + 8].copy_from_slice(&inode.id.to_le_bytes());
    bytes[BLOCK_COUNT_OFFSET..BLOCK_COUNT_OFFSET + 4].copy_from_slice(&block_count.to_le_bytes());

    for (index, block) in inode.blocks.iter().enumerate() {
        let offset = INODE_RECORD_HEADER_LEN + index * 8;
        bytes[offset..offset + 8].copy_from_slice(&block.to_le_bytes());
    }

    let crc = record_crc(&bytes);
    bytes[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
    Ok(bytes)
}

/// Decodes and validates exactly one inode record.
///
/// # Errors
///
/// Returns `UnexpectedEof` for a torn header or payload and `InvalidData` for bad magic, version,
/// kind, reserved fields, inconsistent lengths, checksum mismatch, inode id zero, or duplicate block
/// references.
pub fn decode_inode(bytes: &[u8]) -> io::Result<PersistedInode> {
    if bytes.len() < INODE_RECORD_HEADER_LEN {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "torn inode record header",
        ));
    }
    if bytes[MAGIC_OFFSET..MAGIC_OFFSET + 4] != INODE_RECORD_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid inode record magic",
        ));
    }
    let version = read_u16(bytes, VERSION_OFFSET);
    if version != INODE_RECORD_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported inode record version {version}"),
        ));
    }
    if bytes[RESERVED_OFFSET..INODE_RECORD_HEADER_LEN]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inode record reserved bytes are non-zero",
        ));
    }

    let total_len = usize::try_from(read_u32(bytes, TOTAL_LEN_OFFSET)).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "inode record length is invalid")
    })?;
    let block_count = usize::try_from(read_u32(bytes, BLOCK_COUNT_OFFSET))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "inode block count is invalid"))?;
    let expected_len = INODE_RECORD_HEADER_LEN
        .checked_add(block_count.checked_mul(8).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "inode record size overflow")
        })?)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "inode record size overflow"))?;
    if total_len != expected_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inode record length does not match block count",
        ));
    }
    if bytes.len() < total_len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "torn inode record payload",
        ));
    }
    if bytes.len() != total_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inode decoder requires exactly one record",
        ));
    }

    let stored_crc = read_u32(bytes, CRC_OFFSET);
    if stored_crc != record_crc(bytes) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inode record checksum mismatch",
        ));
    }

    let id = read_u64(bytes, INODE_ID_OFFSET);
    let kind = match read_u16(bytes, KIND_OFFSET) {
        KIND_FILE => InodeKind::File,
        KIND_DIRECTORY => InodeKind::Directory,
        KIND_SYMLINK => InodeKind::Symlink,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid inode kind",
            ))
        }
    };

    let mut blocks = Vec::with_capacity(block_count);
    for index in 0..block_count {
        let offset = INODE_RECORD_HEADER_LEN + index * 8;
        blocks.push(read_u64(bytes, offset));
    }
    validate_inode_fields(id, &blocks)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;

    Ok(PersistedInode { id, kind, blocks })
}

fn validate_inode_fields(id: u64, blocks: &[u64]) -> Result<(), &'static str> {
    if id == 0 {
        return Err("inode identifier zero is reserved");
    }
    let mut sorted = blocks.to_vec();
    sorted.sort_unstable();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("inode record contains duplicate block references");
    }
    Ok(())
}

const fn kind_code(kind: InodeKind) -> u16 {
    match kind {
        InodeKind::File => KIND_FILE,
        InodeKind::Directory => KIND_DIRECTORY,
        InodeKind::Symlink => KIND_SYMLINK,
    }
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

    fn sample() -> PersistedInode {
        PersistedInode::new(7, InodeKind::File, vec![11, 19, 27]).unwrap()
    }

    #[test]
    fn constructor_rejects_invalid_persistence_invariants() {
        let zero = PersistedInode::new(0, InodeKind::File, Vec::new()).unwrap_err();
        assert_eq!(zero.kind(), io::ErrorKind::InvalidInput);

        let duplicate = PersistedInode::new(3, InodeKind::Directory, vec![9, 9]).unwrap_err();
        assert_eq!(duplicate.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn block_range_mutation_is_atomic_and_returns_displaced_blocks() {
        let mut inode = sample();

        let displaced = inode.replace_block_range(1..2, &[41, 43]).unwrap();

        assert_eq!(displaced, vec![19]);
        assert_eq!(inode.blocks, vec![11, 41, 43, 27]);
    }

    #[test]
    fn block_range_mutation_rejects_invalid_candidate_without_changing_inode() {
        let mut inode = sample();
        let original = inode.clone();

        let duplicate = inode.replace_block_range(1..1, &[11]).unwrap_err();
        assert_eq!(duplicate.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(inode, original);

        let outside = inode.replace_block_range(4..4, &[31]).unwrap_err();
        assert_eq!(outside.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(inode, original);
    }

    #[test]
    fn round_trip_preserves_inode() {
        let inode = sample();
        let encoded = encode_inode(&inode).unwrap();
        assert_eq!(decode_inode(&encoded).unwrap(), inode);
    }

    #[test]
    fn round_trip_preserves_symlink_kind() {
        let inode = PersistedInode {
            id: 9,
            kind: InodeKind::Symlink,
            blocks: vec![31],
        };
        let encoded = encode_inode(&inode).unwrap();
        assert_eq!(decode_inode(&encoded).unwrap(), inode);
    }

    #[test]
    fn detects_torn_payload() {
        let encoded = encode_inode(&sample()).unwrap();
        let error = decode_inode(&encoded[..encoded.len() - 1]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn detects_corruption() {
        let mut encoded = encode_inode(&sample()).unwrap();
        *encoded.last_mut().unwrap() ^= 0x80;
        let error = decode_inode(&encoded).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_duplicate_block_references() {
        let inode = PersistedInode {
            id: 3,
            kind: InodeKind::Directory,
            blocks: vec![9, 9],
        };
        let error = encode_inode(&inode).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn rejects_nonzero_reserved_bytes_even_with_recomputed_crc() {
        let mut encoded = encode_inode(&sample()).unwrap();
        encoded[RESERVED_OFFSET] = 1;
        let crc = record_crc(&encoded);
        encoded[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
        let error = decode_inode(&encoded).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
