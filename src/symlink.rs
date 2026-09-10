use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::{load_allocator, store_allocator};
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::{load_directory_table, store_directory_table};
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::{load_inode_table, store_inode_table};
use crate::journal::JournalLog;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::journal_region::store_journal_image;
use crate::recovery::RecoveryReport;
use crate::transaction_image::CaptureDevice;

const SYMLINK_V1_MAGIC: [u8; 4] = *b"SYM1";
const SYMLINK_V1_VERSION: u16 = 1;
const SYMLINK_V1_HEADER_LEN: usize = 12;
const SYMLINK_V1_CRC_OFFSET: usize = 8;
const SYMLINK_V1_MAX_TARGET_LEN: usize = BLOCK_SIZE - SYMLINK_V1_HEADER_LEN;

const SYMLINK_V2_MAGIC: [u8; 4] = *b"SYM2";
const SYMLINK_V2_VERSION: u16 = 1;
const SYMLINK_V2_HEADER_LEN: usize = 16;
const SYMLINK_V2_LEN_OFFSET: usize = 8;
const SYMLINK_V2_CRC_OFFSET: usize = 12;
pub const MAX_SYMLINK_TARGET_BLOCKS: usize = 8;
pub const MAX_SYMLINK_TARGET_LEN: usize =
    MAX_SYMLINK_TARGET_BLOCKS * BLOCK_SIZE - SYMLINK_V2_HEADER_LEN;

/// Creates one durable symbolic link whose UTF-8 target fits in the bounded symlink payload.
///
/// Targets that fit the historical one-block `SYM1` encoding remain encoded as `SYM1`. Longer
/// targets use the backward-compatible `SYM2` multi-block encoding, bounded to
/// [`MAX_SYMLINK_TARGET_BLOCKS`]. The target is an opaque path string. This operation does not
/// resolve it or require that it name an existing inode. Allocation ownership, the symlink inode,
/// the namespace entry, and every target block image are published in one WAL transaction.
///
/// # Errors
/// Returns `InvalidInput` for an empty/oversized target, a missing or non-directory parent, a name
/// collision, inode-id exhaustion, allocator exhaustion, or insufficient journal capacity. Codec,
/// recovery, checkpoint, and block-device failures are propagated.
pub fn create_symlink_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
    target: &str,
) -> io::Result<(u64, RecoveryReport)> {
    let target_images = encode_target_blocks(target)?;
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;

    validate_destination(&inodes, &entries, parent, name)?;
    let inode_id = next_inode_id(&inodes)?;
    let mut blocks = Vec::with_capacity(target_images.len());
    for _ in 0..target_images.len() {
        blocks.push(
            allocator
                .allocate()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
        );
    }

    inodes.push(PersistedInode {
        id: inode_id,
        kind: InodeKind::Symlink,
        blocks: blocks.clone(),
    });
    entries.push(PersistedDirectoryEntry {
        parent,
        target: inode_id,
        name: name.to_owned(),
    });

    let mut changed = collect_metadata_changes(device, superblock, &allocator, &inodes, &entries)?;
    changed.extend(blocks.into_iter().zip(target_images));
    let report = publish_changes(device, superblock, &changed)?;
    Ok((inode_id, report))
}

fn validate_destination(
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
    parent: u64,
    name: &str,
) -> io::Result<()> {
    let parent_inode = inodes
        .iter()
        .find(|inode| inode.id == parent)
        .ok_or_else(|| invalid_input("symlink parent inode is missing"))?;
    if parent_inode.kind != InodeKind::Directory {
        return Err(invalid_input("symlink parent must be a directory"));
    }
    if entries
        .iter()
        .any(|entry| entry.parent == parent && entry.name == name)
    {
        return Err(invalid_input("symlink destination already exists"));
    }
    Ok(())
}

fn next_inode_id(inodes: &[PersistedInode]) -> io::Result<u64> {
    inodes
        .iter()
        .map(|inode| inode.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| invalid_input("symlink inode identifier space exhausted"))
}

fn collect_metadata_changes(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    allocator: &BlockAllocator,
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
) -> io::Result<Vec<(u64, [u8; BLOCK_SIZE])>> {
    let mut capture = CaptureDevice::new(superblock.total_blocks);
    store_allocator(&mut capture, superblock, allocator)?;
    store_inode_table(&mut capture, superblock, inodes)?;
    store_directory_table(&mut capture, superblock, entries)?;

    let mut changed = Vec::new();
    capture.collect_changed_range(
        device,
        superblock.allocation_range(),
        "symlink image did not render every allocation metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.inode_range(),
        "symlink image did not render every inode metadata block",
        &mut changed,
    )?;
    capture.collect_changed_range(
        device,
        superblock.directory_range(),
        "symlink image did not render every directory metadata block",
        &mut changed,
    )?;
    capture.ensure_empty(
        "symlink image rendered outside allocation, inode, and directory metadata regions",
    )?;
    Ok(changed)
}

fn publish_changes(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    changed: &[(u64, [u8; BLOCK_SIZE])],
) -> io::Result<RecoveryReport> {
    let mut log = JournalLog::new();
    let txid = log.begin()?;
    for (home_block, image) in changed.iter().copied() {
        log.write(txid, home_block, image)?;
    }
    log.commit(txid)?;
    store_journal_image(device, *superblock, log.entries())?;
    let report = recover_journal_and_checkpoint(device, *superblock)?;
    if report.committed_transactions != 1 || report.home_writes != changed.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "symlink recovery report is inconsistent",
        ));
    }
    Ok(report)
}

/// Reads and validates the opaque target string of one persisted symbolic-link inode.
///
/// Historical one-block `SYM1` payloads remain readable. New `SYM2` payloads may span multiple
/// inode blocks and carry one length/checksum contract over the complete target bytes.
///
/// # Errors
/// Returns `InvalidInput` for a missing/non-symlink inode. An empty block vector, too many blocks,
/// inconsistent codec/block count, corrupt payload, or invalid UTF-8 returns `InvalidData`.
pub fn read_symlink(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
) -> io::Result<String> {
    let inodes = load_inode_table(device, superblock)?;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| invalid_input("symlink inode is missing"))?;
    if inode.kind != InodeKind::Symlink {
        return Err(invalid_input("inode is not a symbolic link"));
    }
    read_symlink_inode(device, inode)
}

pub(crate) fn validate_symlink_inode(
    device: &mut impl BlockDevice,
    inode: &PersistedInode,
) -> io::Result<()> {
    if inode.kind != InodeKind::Symlink {
        return Ok(());
    }
    read_symlink_inode(device, inode).map(|_| ())
}

fn read_symlink_inode(device: &mut impl BlockDevice, inode: &PersistedInode) -> io::Result<String> {
    if inode.blocks.is_empty() {
        return Err(invalid_data(
            "symbolic link must reference at least one block",
        ));
    }
    if inode.blocks.len() > MAX_SYMLINK_TARGET_BLOCKS {
        return Err(invalid_data("symbolic link exceeds bounded block limit"));
    }

    let mut images = Vec::with_capacity(inode.blocks.len());
    for block in &inode.blocks {
        let mut image = [0_u8; BLOCK_SIZE];
        device.read_block(*block, &mut image)?;
        images.push(image);
    }

    if images[0][..4] == SYMLINK_V1_MAGIC {
        if images.len() != 1 {
            return Err(invalid_data(
                "SYM1 symbolic link must reference exactly one block",
            ));
        }
        decode_v1_target(&images[0])
    } else if images[0][..4] == SYMLINK_V2_MAGIC {
        decode_v2_target(&images)
    } else {
        Err(invalid_data("invalid symlink payload magic"))
    }
}

fn encode_target_blocks(target: &str) -> io::Result<Vec<[u8; BLOCK_SIZE]>> {
    let target = target.as_bytes();
    if target.is_empty() {
        return Err(invalid_input("symlink target must not be empty"));
    }
    if target.len() <= SYMLINK_V1_MAX_TARGET_LEN {
        return Ok(vec![encode_v1_target(target)?]);
    }
    if target.len() > MAX_SYMLINK_TARGET_LEN {
        return Err(invalid_input(
            "symlink target exceeds bounded multi-block limit",
        ));
    }

    let total_len = u32::try_from(target.len())
        .map_err(|_| invalid_input("symlink target length exceeds codec limit"))?;
    let encoded_len = SYMLINK_V2_HEADER_LEN
        .checked_add(target.len())
        .ok_or_else(|| invalid_input("symlink target encoded length overflow"))?;
    let block_count = encoded_len.div_ceil(BLOCK_SIZE);
    if block_count > MAX_SYMLINK_TARGET_BLOCKS {
        return Err(invalid_input(
            "symlink target exceeds bounded multi-block limit",
        ));
    }

    let mut bytes = vec![0_u8; block_count * BLOCK_SIZE];
    bytes[..4].copy_from_slice(&SYMLINK_V2_MAGIC);
    bytes[4..6].copy_from_slice(&SYMLINK_V2_VERSION.to_le_bytes());
    bytes[SYMLINK_V2_LEN_OFFSET..SYMLINK_V2_LEN_OFFSET + 4]
        .copy_from_slice(&total_len.to_le_bytes());
    bytes[SYMLINK_V2_HEADER_LEN..SYMLINK_V2_HEADER_LEN + target.len()].copy_from_slice(target);
    let crc = target_crc(target);
    bytes[SYMLINK_V2_CRC_OFFSET..SYMLINK_V2_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());

    Ok(bytes
        .chunks_exact(BLOCK_SIZE)
        .map(|chunk| {
            let mut image = [0_u8; BLOCK_SIZE];
            image.copy_from_slice(chunk);
            image
        })
        .collect())
}

fn encode_v1_target(target: &[u8]) -> io::Result<[u8; BLOCK_SIZE]> {
    let len = u16::try_from(target.len())
        .map_err(|_| invalid_input("symlink target length exceeds codec limit"))?;
    let mut image = [0_u8; BLOCK_SIZE];
    image[..4].copy_from_slice(&SYMLINK_V1_MAGIC);
    image[4..6].copy_from_slice(&SYMLINK_V1_VERSION.to_le_bytes());
    image[6..8].copy_from_slice(&len.to_le_bytes());
    image[SYMLINK_V1_HEADER_LEN..SYMLINK_V1_HEADER_LEN + target.len()].copy_from_slice(target);
    let crc = v1_symlink_crc(&image);
    image[SYMLINK_V1_CRC_OFFSET..SYMLINK_V1_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
    Ok(image)
}

fn decode_v1_target(image: &[u8; BLOCK_SIZE]) -> io::Result<String> {
    if image[..4] != SYMLINK_V1_MAGIC {
        return Err(invalid_data("invalid symlink payload magic"));
    }
    if u16::from_le_bytes([image[4], image[5]]) != SYMLINK_V1_VERSION {
        return Err(invalid_data("unsupported symlink payload version"));
    }
    let len = usize::from(u16::from_le_bytes([image[6], image[7]]));
    if len == 0 || len > SYMLINK_V1_MAX_TARGET_LEN {
        return Err(invalid_data("invalid symlink target length"));
    }
    let stored_crc = u32::from_le_bytes([image[8], image[9], image[10], image[11]]);
    if stored_crc != v1_symlink_crc(image) {
        return Err(invalid_data("symlink target checksum mismatch"));
    }
    if image[SYMLINK_V1_HEADER_LEN + len..]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(invalid_data(
            "symlink target block has non-zero trailing bytes",
        ));
    }
    decode_utf8(&image[SYMLINK_V1_HEADER_LEN..SYMLINK_V1_HEADER_LEN + len])
}

fn decode_v2_target(images: &[[u8; BLOCK_SIZE]]) -> io::Result<String> {
    let first = &images[0];
    if u16::from_le_bytes([first[4], first[5]]) != SYMLINK_V2_VERSION {
        return Err(invalid_data(
            "unsupported multi-block symlink payload version",
        ));
    }
    if first[6..8].iter().any(|byte| *byte != 0) {
        return Err(invalid_data(
            "multi-block symlink reserved bytes are non-zero",
        ));
    }
    let len = usize::try_from(u32::from_le_bytes([
        first[8], first[9], first[10], first[11],
    ]))
    .map_err(|_| invalid_data("multi-block symlink target length is invalid"))?;
    if len <= SYMLINK_V1_MAX_TARGET_LEN || len > MAX_SYMLINK_TARGET_LEN {
        return Err(invalid_data("invalid multi-block symlink target length"));
    }
    let encoded_len = SYMLINK_V2_HEADER_LEN
        .checked_add(len)
        .ok_or_else(|| invalid_data("multi-block symlink encoded length overflow"))?;
    if encoded_len.div_ceil(BLOCK_SIZE) != images.len() {
        return Err(invalid_data(
            "multi-block symlink block count does not match target length",
        ));
    }

    let mut bytes = Vec::with_capacity(images.len() * BLOCK_SIZE);
    for image in images {
        bytes.extend_from_slice(image);
    }
    if bytes[encoded_len..].iter().any(|byte| *byte != 0) {
        return Err(invalid_data(
            "multi-block symlink has non-zero trailing bytes",
        ));
    }
    let target = &bytes[SYMLINK_V2_HEADER_LEN..encoded_len];
    let stored_crc = u32::from_le_bytes([first[12], first[13], first[14], first[15]]);
    if stored_crc != target_crc(target) {
        return Err(invalid_data("multi-block symlink target checksum mismatch"));
    }
    decode_utf8(target)
}

fn decode_utf8(target: &[u8]) -> io::Result<String> {
    std::str::from_utf8(target)
        .map(str::to_owned)
        .map_err(|_| invalid_data("symlink target is not valid UTF-8"))
}

fn v1_symlink_crc(image: &[u8; BLOCK_SIZE]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for (index, byte) in image.iter().enumerate() {
        let value = if (SYMLINK_V1_CRC_OFFSET..SYMLINK_V1_CRC_OFFSET + 4).contains(&index) {
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

fn target_crc(bytes: &[u8]) -> u32 {
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

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_block_target_codec_round_trips_and_detects_corruption() {
        let mut images = encode_target_blocks("../target/file").unwrap();
        assert_eq!(decode_v1_target(&images[0]).unwrap(), "../target/file");

        images[0][SYMLINK_V1_HEADER_LEN] ^= 0x20;
        assert_eq!(
            decode_v1_target(&images[0]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn multi_block_target_codec_round_trips_and_detects_corruption() {
        let target = format!("/{}", "segment/".repeat(700));
        let mut images = encode_target_blocks(&target).unwrap();
        assert!(images.len() > 1);
        assert_eq!(decode_v2_target(&images).unwrap(), target);

        images[1][0] ^= 0x20;
        assert_eq!(
            decode_v2_target(&images).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn target_codec_rejects_empty_and_oversized_targets() {
        assert_eq!(
            encode_target_blocks("").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let oversized = "x".repeat(MAX_SYMLINK_TARGET_LEN + 1);
        assert_eq!(
            encode_target_blocks(&oversized).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
