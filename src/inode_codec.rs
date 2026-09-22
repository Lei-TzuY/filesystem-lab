use std::io;
use std::ops::Range;

use crate::block::BLOCK_SIZE_U64;
use crate::inode::{Inode, InodeKind};

pub const INODE_RECORD_MAGIC: [u8; 4] = *b"INO1";
pub const INODE_RECORD_VERSION: u16 = 3;
pub const INODE_RECORD_HEADER_LEN: usize = 40;

const KIND_FILE: u16 = 1;
const KIND_DIRECTORY: u16 = 2;
const KIND_SYMLINK: u16 = 3;
const MAGIC_OFFSET: usize = 0;
const VERSION_OFFSET: usize = 4;
const KIND_OFFSET: usize = 6;
const TOTAL_LEN_OFFSET: usize = 8;
const INODE_ID_OFFSET: usize = 12;
const BLOCK_COUNT_OFFSET: usize = 20;
const BYTE_LEN_OFFSET: usize = 24;
const CRC_OFFSET: usize = 32;
const RESERVED_OFFSET: usize = 36;

#[derive(Debug, Clone)]
pub struct PersistedInode {
    pub id: u64,
    pub kind: InodeKind,
    pub blocks: Vec<u64>,
    /// Exact regular-file EOF in bytes.
    ///
    /// New production values and decoded v3 records are canonical. A zero value on a non-empty
    /// regular file is accepted only as an in-memory compatibility shorthand for older direct
    /// struct literals and is canonicalized to full block capacity before persistence.
    pub byte_len: u64,
}

impl PartialEq for PersistedInode {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
            && self.kind == other.kind
            && self.blocks == other.blocks
            && self.canonical_byte_len().ok() == other.canonical_byte_len().ok()
    }
}

impl Eq for PersistedInode {}

impl PersistedInode {
    /// Constructs one persistence-safe inode value.
    ///
    /// Regular files created through this compatibility constructor use their complete logical-block
    /// capacity as EOF. Directory and symlink records use byte length zero.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the inode identifier is zero, block references are duplicated,
    /// or the derived regular-file size overflows.
    pub fn new(id: u64, kind: InodeKind, blocks: Vec<u64>) -> io::Result<Self> {
        let byte_len = default_byte_len(kind, &blocks)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        validate_inode_fields(id, kind, &blocks, byte_len, false)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        Ok(Self {
            id,
            kind,
            blocks,
            byte_len,
        })
    }

    /// Constructs a regular-file inode with an exact persisted EOF.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the identifier or block mapping is invalid, when a zero-block
    /// file has non-zero size, or when EOF does not lie inside the final referenced block.
    pub fn new_file_with_size(id: u64, blocks: Vec<u64>, byte_len: u64) -> io::Result<Self> {
        validate_inode_fields(id, InodeKind::File, &blocks, byte_len, false)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        Ok(Self {
            id,
            kind: InodeKind::File,
            blocks,
            byte_len,
        })
    }

    /// Returns the canonical exact byte length represented by this inode.
    ///
    /// The zero-on-nonempty compatibility shorthand is normalized to full block capacity here.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when durable inode invariants are invalid.
    pub fn canonical_byte_len(&self) -> io::Result<u64> {
        validate_inode_fields(self.id, self.kind, &self.blocks, self.byte_len, true)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))
    }

    /// Validates invariants that must hold before an inode can be encoded durably.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for an invalid identifier, duplicate block references, or an invalid
    /// regular-file EOF.
    pub fn validate_for_persistence(&self) -> io::Result<()> {
        self.canonical_byte_len().map(|_| ())
    }

    /// Replaces one logical block range while preserving durable inode invariants and EOF position.
    ///
    /// Block-granular insert/remove operations preserve the current unused tail length in the final
    /// block. Equal-length replacements therefore preserve EOF exactly; inserting or removing whole
    /// blocks shifts EOF by the same whole-block byte count.
    ///
    /// The returned vector contains the displaced blocks in logical order.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` when the range is reversed or outside the current block vector, the
    /// resulting mapping contains duplicate block references, or byte-length arithmetic overflows.
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

        let current_byte_len = self.canonical_byte_len()?;
        let current_capacity = block_capacity(&self.blocks)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        let tail_slack = current_capacity
            .checked_sub(current_byte_len)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "inode EOF exceeds capacity"))?;

        let displaced = self.blocks[range.clone()].to_vec();
        let mut candidate = self.blocks.clone();
        candidate.splice(range, replacements.iter().copied());

        let candidate_byte_len = if self.kind == InodeKind::File {
            if candidate.is_empty() {
                0
            } else {
                let capacity = block_capacity(&candidate)
                    .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
                capacity.checked_sub(tail_slack).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "inode block mutation cannot preserve EOF tail offset",
                    )
                })?
            }
        } else {
            0
        };

        validate_inode_fields(
            self.id,
            self.kind,
            &candidate,
            candidate_byte_len,
            false,
        )
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        self.blocks = candidate;
        self.byte_len = candidate_byte_len;
        Ok(displaced)
    }

    /// Sets an exact regular-file EOF without changing the block mapping.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for non-file inodes or an EOF not representable by the current
    /// non-sparse block mapping.
    pub fn set_file_byte_len(&mut self, byte_len: u64) -> io::Result<()> {
        if self.kind != InodeKind::File {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "only regular files have byte EOF",
            ));
        }
        validate_inode_fields(self.id, self.kind, &self.blocks, byte_len, false)
            .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
        self.byte_len = byte_len;
        Ok(())
    }
}

impl From<&Inode> for PersistedInode {
    fn from(inode: &Inode) -> Self {
        Self::new(inode.id().get(), inode.kind(), inode.blocks().to_vec())
            .expect("validated in-memory inode converts to persisted inode")
    }
}

/// Encodes one inode into a self-delimiting, checksummed little-endian version-3 record.
///
/// # Errors
///
/// Returns `InvalidInput` when inode invariants fail, the block count cannot fit in the record
/// header, or encoded-length arithmetic overflows.
pub fn encode_inode(inode: &PersistedInode) -> io::Result<Vec<u8>> {
    let byte_len = inode.canonical_byte_len()?;
    let block_count = u32::try_from(inode.blocks.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "inode block count exceeds codec limit",
        )
    })?;
    let payload_len = inode
        .blocks
        .len()
        .checked_mul(8)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "inode record size overflow"))?;
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
    bytes[BYTE_LEN_OFFSET..BYTE_LEN_OFFSET + 8].copy_from_slice(&byte_len.to_le_bytes());

    for (index, block) in inode.blocks.iter().enumerate() {
        let offset = INODE_RECORD_HEADER_LEN + index * 8;
        bytes[offset..offset + 8].copy_from_slice(&block.to_le_bytes());
    }

    let crc = record_crc(&bytes);
    bytes[CRC_OFFSET..CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
    Ok(bytes)
}

/// Decodes and validates exactly one version-3 inode record.
///
/// # Errors
///
/// Returns `UnexpectedEof` for a torn header or payload and `InvalidData` for bad magic/version,
/// kind, reserved fields, inconsistent lengths, checksum mismatch, inode id zero, duplicate block
/// references, or an impossible byte EOF.
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
    let byte_len = read_u64(bytes, BYTE_LEN_OFFSET);

    let mut blocks = Vec::with_capacity(block_count);
    for index in 0..block_count {
        let offset = INODE_RECORD_HEADER_LEN + index * 8;
        blocks.push(read_u64(bytes, offset));
    }
    validate_inode_fields(id, kind, &blocks, byte_len, false)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;

    Ok(PersistedInode {
        id,
        kind,
        blocks,
        byte_len,
    })
}

fn default_byte_len(kind: InodeKind, blocks: &[u64]) -> Result<u64, &'static str> {
    if kind == InodeKind::File {
        block_capacity(blocks)
    } else {
        Ok(0)
    }
}

fn block_capacity(blocks: &[u64]) -> Result<u64, &'static str> {
    let count = u64::try_from(blocks.len()).map_err(|_| "inode block count exceeds u64")?;
    count
        .checked_mul(BLOCK_SIZE_U64)
        .ok_or("inode byte capacity overflow")
}

fn validate_inode_fields(
    id: u64,
    kind: InodeKind,
    blocks: &[u64],
    byte_len: u64,
    allow_full_capacity_sentinel: bool,
) -> Result<u64, &'static str> {
    if id == 0 {
        return Err("inode identifier zero is reserved");
    }

    let mut sorted = blocks.to_vec();
    sorted.sort_unstable();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("inode record contains duplicate block references");
    }

    if kind != InodeKind::File {
        if byte_len != 0 {
            return Err("non-file inode byte length must be zero");
        }
        return Ok(0);
    }

    let capacity = block_capacity(blocks)?;
    if blocks.is_empty() {
        if byte_len != 0 {
            return Err("zero-block regular file must have zero byte length");
        }
        return Ok(0);
    }

    let canonical = if byte_len == 0 && allow_full_capacity_sentinel {
        capacity
    } else {
        byte_len
    };
    if canonical == 0 || canonical > capacity {
        return Err("regular-file byte length exceeds block capacity");
    }
    if canonical <= capacity - BLOCK_SIZE_U64 {
        return Err("regular-file EOF must lie inside its final referenced block");
    }
    Ok(canonical)
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
            byte_len: 0,
        };
        let encoded = encode_inode(&inode).unwrap();
        assert_eq!(decode_inode(&encoded).unwrap(), inode);
    }

    #[test]
    fn round_trip_preserves_partial_regular_file_eof() {
        let inode = PersistedInode::new_file_with_size(11, vec![41, 43], 5000).unwrap();

        let encoded = encode_inode(&inode).unwrap();
        let decoded = decode_inode(&encoded).unwrap();

        assert_eq!(decoded, inode);
        assert_eq!(decoded.byte_len, 5000);
        assert_eq!(encoded.len(), INODE_RECORD_HEADER_LEN + 16);
    }

    #[test]
    fn rejects_impossible_regular_file_eof() {
        assert_eq!(
            PersistedInode::new_file_with_size(11, vec![41], 0)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            PersistedInode::new_file_with_size(11, vec![41], BLOCK_SIZE_U64 + 1)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            PersistedInode::new_file_with_size(11, vec![41, 43], BLOCK_SIZE_U64)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn block_range_mutation_preserves_partial_tail_offset() {
        let mut inode = PersistedInode::new_file_with_size(11, vec![41, 43], 5000).unwrap();

        inode.replace_block_range(0..0, &[47]).unwrap();

        assert_eq!(inode.blocks, vec![47, 41, 43]);
        assert_eq!(inode.byte_len, 5000 + BLOCK_SIZE_U64);
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
            byte_len: 0,
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
