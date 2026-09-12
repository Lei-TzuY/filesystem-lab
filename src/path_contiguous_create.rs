use std::collections::HashSet;
use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::create_data_tx::store_create_with_blocks_journaled;
use crate::directory_codec::{encode_directory_entry, PersistedDirectoryEntry};
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Creates one durable regular file whose initial logical blocks occupy one contiguous physical run.
///
/// `data` must contain at least one complete 4 KiB logical block. The destination parent follows the
/// existing bounded symbolic-link resolver, while the final component is created as a new regular
/// file. After recovery establishes the current home state, the allocator reserves the lowest
/// numbered contiguous free run large enough for every supplied logical block. Allocation, inode,
/// namespace, and all initial data images are then published through one bounded WAL transaction.
///
/// The contiguous run is an allocation policy guarantee for this creation operation only. Format v5
/// still persists the inode's explicit block vector and does not gain an extent record, sparse-file
/// semantics, or a durable contiguity invariant. Later file mutations may therefore make the mapping
/// non-contiguous.
///
/// Recovery exposes either no destination or the complete initialized file. If total free space is
/// sufficient but fragmented so that no run can satisfy `data.len()`, the operation fails before WAL
/// publication and leaves allocator, inode, namespace, and file-data state unchanged.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty block list, malformed destination, non-directory parent,
/// namespace collision, invalid entry name, inode-ID exhaustion, lack of a sufficiently large
/// contiguous free run, or journal-capacity exhaustion. Metadata corruption and durable device I/O
/// errors are propagated.
pub fn create_contiguous_file_with_blocks_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    destination: &str,
    data: &[[u8; BLOCK_SIZE]],
) -> io::Result<(u64, RecoveryReport)> {
    if data.is_empty() {
        return Err(invalid_input(
            "contiguous multi-block file create requires at least one logical block",
        ));
    }
    let block_count = u64::try_from(data.len()).map_err(|_| {
        invalid_input("contiguous multi-block file create exceeds the block address space")
    })?;

    let (parent_path, name) = split_destination(destination)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;

    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;
    validate_create_destination(parent, name, &inodes, &entries)?;
    let inode_id = next_inode_id(&inodes)?;

    let first_block = allocator
        .allocate_contiguous(block_count)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let mut initialized = Vec::with_capacity(data.len());
    let mut inode_blocks = Vec::with_capacity(data.len());
    for (index, image) in data.iter().enumerate() {
        let offset = u64::try_from(index).map_err(|_| {
            invalid_input("contiguous multi-block file create exceeds the block address space")
        })?;
        let block = first_block
            .checked_add(offset)
            .ok_or_else(|| invalid_data("contiguous allocation run overflowed block address space"))?;
        inode_blocks.push(block);
        initialized.push((block, *image));
    }

    let new_entry = PersistedDirectoryEntry {
        parent,
        target: inode_id,
        name: name.to_owned(),
    };
    encode_directory_entry(&new_entry)?;
    inodes.push(PersistedInode {
        id: inode_id,
        kind: InodeKind::File,
        blocks: inode_blocks,
    });
    entries.push(new_entry);

    let report = store_create_with_blocks_journaled(
        device,
        superblock,
        &allocator,
        &inodes,
        &entries,
        &initialized,
    )?;
    Ok((inode_id, report))
}

fn validate_create_destination(
    parent: u64,
    name: &str,
    inodes: &[PersistedInode],
    entries: &[PersistedDirectoryEntry],
) -> io::Result<()> {
    let parent_inode = inodes
        .iter()
        .find(|inode| inode.id == parent)
        .ok_or_else(|| invalid_data("resolved parent inode is missing from inode table"))?;
    if parent_inode.kind != InodeKind::Directory {
        return Err(invalid_input("create parent pathname is not a directory"));
    }
    if entries
        .iter()
        .any(|entry| entry.parent == parent && entry.name == name)
    {
        return Err(invalid_input("create destination already exists"));
    }
    encode_directory_entry(&PersistedDirectoryEntry {
        parent,
        target: 1,
        name: name.to_owned(),
    })?;
    Ok(())
}

fn next_inode_id(inodes: &[PersistedInode]) -> io::Result<u64> {
    let mut seen = HashSet::with_capacity(inodes.len());
    let mut max_inode_id = 0_u64;
    for inode in inodes {
        if !seen.insert(inode.id) {
            return Err(invalid_data("inode table contains duplicate identifiers"));
        }
        max_inode_id = max_inode_id.max(inode.id);
    }
    max_inode_id
        .checked_add(1)
        .filter(|id| *id != 0)
        .ok_or_else(|| invalid_input("no fresh inode identifier is available"))
}

fn split_destination(path: &str) -> io::Result<(&str, &str)> {
    if !path.starts_with('/') {
        return Err(invalid_input("create destination must be an absolute path"));
    }
    if path == "/" {
        return Err(invalid_input("create destination cannot be the root path"));
    }

    let (parent, name) = path
        .rsplit_once('/')
        .ok_or_else(|| invalid_input("create destination must contain a final component"))?;
    if name.is_empty() {
        return Err(invalid_input("create destination final component is empty"));
    }

    Ok((if parent.is_empty() { "/" } else { parent }, name))
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
