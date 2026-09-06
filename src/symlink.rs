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

const SYMLINK_MAGIC: [u8; 4] = *b"SYM1";
const SYMLINK_VERSION: u16 = 1;
const SYMLINK_HEADER_LEN: usize = 12;
const SYMLINK_CRC_OFFSET: usize = 8;
pub const MAX_SYMLINK_TARGET_LEN: usize = BLOCK_SIZE - SYMLINK_HEADER_LEN;

/// Creates one durable symbolic link whose UTF-8 target fits in one filesystem data block.
///
/// The target is an opaque path string. This operation does not resolve it or require that it name
/// an existing inode. Allocation ownership, the symlink inode, the namespace entry, and the target
/// block image are published in one WAL transaction.
///
/// # Errors
/// Returns `InvalidInput` for an empty/oversized target, a missing or non-directory parent, a name
/// collision, inode-id exhaustion, or allocator exhaustion. Codec, journal, recovery, checkpoint,
/// and block-device failures are propagated.
pub fn create_symlink_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
    target: &str,
) -> io::Result<(u64, RecoveryReport)> {
    let target_image = encode_target(target)?;
    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;

    validate_destination(&inodes, &entries, parent, name)?;
    let inode_id = next_inode_id(&inodes)?;
    let block = allocator
        .allocate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

    inodes.push(PersistedInode {
        id: inode_id,
        kind: InodeKind::Symlink,
        blocks: vec![block],
    });
    entries.push(PersistedDirectoryEntry {
        parent,
        target: inode_id,
        name: name.to_owned(),
    });

    let mut changed = collect_metadata_changes(
        device,
        superblock,
        &allocator,
        &inodes,
        &entries,
    )?;
    changed.push((block, target_image));
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
/// # Errors
/// Returns `InvalidInput` for a missing/non-symlink inode or for a symlink that does not reference
/// exactly one block. Corrupt target payloads return `InvalidData`.
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
    if inode.blocks.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "symbolic link must reference exactly one data block",
        ));
    }
    let mut image = [0_u8; BLOCK_SIZE];
    device.read_block(inode.blocks[0], &mut image)?;
    decode_target(&image)
}

pub(crate) fn validate_symlink_inode(
    device: &mut impl BlockDevice,
    inode: &PersistedInode,
) -> io::Result<()> {
    if inode.kind != InodeKind::Symlink {
        return Ok(());
    }
    if inode.blocks.len() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "symlink inode {} must reference exactly one block",
                inode.id
            ),
        ));
    }
    let mut image = [0_u8; BLOCK_SIZE];
    device.read_block(inode.blocks[0], &mut image)?;
    decode_target(&image).map(|_| ())
}

fn encode_target(target: &str) -> io::Result<[u8; BLOCK_SIZE]> {
    let target = target.as_bytes();
    if target.is_empty() {
        return Err(invalid_input("symlink target must not be empty"));
    }
    if target.len() > MAX_SYMLINK_TARGET_LEN {
        return Err(invalid_input("symlink target exceeds one-block limit"));
    }
    let len = u16::try_from(target.len())
        .map_err(|_| invalid_input("symlink target length exceeds codec limit"))?;
    let mut image = [0_u8; BLOCK_SIZE];
    image[..4].copy_from_slice(&SYMLINK_MAGIC);
    image[4..6].copy_from_slice(&SYMLINK_VERSION.to_le_bytes());
    image[6..8].copy_from_slice(&len.to_le_bytes());
    image[SYMLINK_HEADER_LEN..SYMLINK_HEADER_LEN + target.len()].copy_from_slice(target);
    let crc = symlink_crc(&image);
    image[SYMLINK_CRC_OFFSET..SYMLINK_CRC_OFFSET + 4].copy_from_slice(&crc.to_le_bytes());
    Ok(image)
}

fn decode_target(image: &[u8; BLOCK_SIZE]) -> io::Result<String> {
    if image[..4] != SYMLINK_MAGIC {
        return Err(invalid_data("invalid symlink payload magic"));
    }
    if u16::from_le_bytes([image[4], image[5]]) != SYMLINK_VERSION {
        return Err(invalid_data("unsupported symlink payload version"));
    }
    let len = usize::from(u16::from_le_bytes([image[6], image[7]]));
    if len == 0 || len > MAX_SYMLINK_TARGET_LEN {
        return Err(invalid_data("invalid symlink target length"));
    }
    let stored_crc = u32::from_le_bytes([image[8], image[9], image[10], image[11]]);
    if stored_crc != symlink_crc(image) {
        return Err(invalid_data("symlink target checksum mismatch"));
    }
    if image[SYMLINK_HEADER_LEN + len..]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err(invalid_data(
            "symlink target block has non-zero trailing bytes",
        ));
    }
    let target = std::str::from_utf8(&image[SYMLINK_HEADER_LEN..SYMLINK_HEADER_LEN + len])
        .map_err(|_| invalid_data("symlink target is not valid UTF-8"))?;
    Ok(target.to_owned())
}

fn symlink_crc(image: &[u8; BLOCK_SIZE]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for (index, byte) in image.iter().enumerate() {
        let value = if (SYMLINK_CRC_OFFSET..SYMLINK_CRC_OFFSET + 4).contains(&index) {
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
    fn target_codec_round_trips_and_detects_corruption() {
        let image = encode_target("../target/file").unwrap();
        assert_eq!(decode_target(&image).unwrap(), "../target/file");

        let mut corrupt = image;
        corrupt[SYMLINK_HEADER_LEN] ^= 0x20;
        assert_eq!(
            decode_target(&corrupt).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn target_codec_rejects_empty_and_oversized_targets() {
        assert_eq!(
            encode_target("").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        let oversized = "x".repeat(MAX_SYMLINK_TARGET_LEN + 1);
        assert_eq!(
            encode_target(&oversized).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
