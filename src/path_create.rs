use std::collections::HashSet;
use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::create_data_tx::{store_create_with_blocks_journaled, store_create_with_data_journaled};
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_codec::{encode_directory_entry, PersistedDirectoryEntry};
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

/// Creates one durable zero-block regular file at an absolute pathname.
///
/// The destination parent is resolved with the existing bounded symbolic-link rules. The final
/// component is not resolved: it becomes one new durable directory entry naming a freshly assigned
/// regular-file inode. Format v5 has no persisted byte length, so the created file starts with an
/// empty logical-block vector and therefore owns no data blocks.
///
/// Before reading persistent allocator, inode, or namespace state, pathname create recovers and
/// checkpoints any older durable journal image. That guarantees the mutation is recomputed from
/// recovered home state rather than from a partial post-crash home-write prefix.
///
/// The inode-table and directory-table changes are published together through the existing create
/// WAL transaction. The allocator image is passed through unchanged, which makes zero-block create
/// preserve exact allocated/free accounting.
///
/// # Errors
///
/// Returns `InvalidInput` for malformed destinations, a non-directory parent, namespace collision,
/// invalid directory-entry name, or exhausted inode identifiers. Returns `InvalidData` when the
/// persisted inode table contains duplicate identifiers. Parent-resolution, metadata decoding, WAL,
/// recovery, checkpoint, and device I/O errors are propagated.
pub fn create_empty_file_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    destination: &str,
) -> io::Result<(u64, RecoveryReport)> {
    create_blockless_inode_at_path_journaled(device, superblock, destination, InodeKind::File)
}

/// Creates one durable regular file with exactly one initialized data block at an absolute pathname.
///
/// This remains the compatibility entry point for the original one-block create contract and
/// delegates publication to the same bounded transaction used by multi-block creation.
///
/// # Errors
///
/// Returns the same validation and durable I/O errors as [`create_file_with_blocks_at_path_journaled`].
pub fn create_one_block_file_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    destination: &str,
    data: &[u8; BLOCK_SIZE],
) -> io::Result<(u64, RecoveryReport)> {
    let (parent_path, name) = split_destination(destination)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;

    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;
    validate_create_destination(parent, name, &inodes, &entries)?;
    let inode_id = next_inode_id(&inodes)?;
    let data_block = allocator
        .allocate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

    let new_entry = PersistedDirectoryEntry {
        parent,
        target: inode_id,
        name: name.to_owned(),
    };
    encode_directory_entry(&new_entry)?;
    inodes.push(PersistedInode {
        id: inode_id,
        kind: InodeKind::File,
        blocks: vec![data_block],
    });
    entries.push(new_entry);

    let report = store_create_with_data_journaled(
        device, superblock, &allocator, &inodes, &entries, data_block, data,
    )?;
    Ok((inode_id, report))
}

/// Creates one durable regular file with multiple initialized logical blocks at an absolute pathname.
///
/// `data` must contain at least one complete 4 KiB logical block. After pathname validation, any
/// older committed WAL is recovered and checkpointed before parent resolution. The operation then
/// allocates one distinct physical block per logical block, appends those references to one fresh
/// regular-file inode in input order, and publishes allocation, inode, namespace, and every initial
/// data image under one bounded WAL commit.
///
/// Recovery therefore exposes either no destination at all or the complete file with every initial
/// block initialized. No prefix file is a valid committed outcome. Format v5 is unchanged: because
/// it has no persisted byte EOF, the API accepts only complete logical blocks and does not imply
/// sparse or partial-block length semantics.
///
/// # Errors
///
/// Returns `InvalidInput` for an empty block list, malformed destination, non-directory parent,
/// namespace collision, invalid entry name, inode-ID exhaustion, allocator exhaustion, or journal
/// capacity exhaustion. Metadata corruption and durable device errors are propagated.
pub fn create_file_with_blocks_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    destination: &str,
    data: &[[u8; BLOCK_SIZE]],
) -> io::Result<(u64, RecoveryReport)> {
    if data.is_empty() {
        return Err(invalid_input(
            "multi-block file create requires at least one logical block",
        ));
    }

    let (parent_path, name) = split_destination(destination)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;

    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;
    validate_create_destination(parent, name, &inodes, &entries)?;
    let inode_id = next_inode_id(&inodes)?;

    let mut initialized = Vec::with_capacity(data.len());
    let mut inode_blocks = Vec::with_capacity(data.len());
    for image in data {
        let block = allocator
            .allocate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
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

/// Creates one durable empty directory at an absolute pathname.
///
/// The destination parent is resolved with the existing bounded symbolic-link rules. The final
/// component is not resolved: it becomes one new durable directory entry naming a freshly assigned
/// directory inode. Format v5 represents an empty directory with no child entries and no data
/// blocks, so allocator ownership is preserved exactly.
///
/// Before reading persistent allocator, inode, or namespace state, pathname create recovers and
/// checkpoints any older durable journal image. Recovery therefore establishes the state from which
/// the new directory mutation is computed.
///
/// The new inode and parent namespace entry are published together through the existing create WAL
/// transaction. Recovery therefore exposes either the complete old namespace or the complete new
/// empty directory, never an inode-only or directory-entry-only state.
///
/// # Errors
///
/// Returns `InvalidInput` for malformed destinations, a non-directory parent, namespace collision,
/// invalid directory-entry name, or exhausted inode identifiers. Returns `InvalidData` when the
/// persisted inode table contains duplicate identifiers. Parent-resolution, metadata decoding, WAL,
/// recovery, checkpoint, and device I/O errors are propagated.
pub fn create_directory_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    destination: &str,
) -> io::Result<(u64, RecoveryReport)> {
    create_blockless_inode_at_path_journaled(device, superblock, destination, InodeKind::Directory)
}

fn create_blockless_inode_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    destination: &str,
    kind: InodeKind,
) -> io::Result<(u64, RecoveryReport)> {
    let (parent_path, name) = split_destination(destination)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    let parent = resolve_path_following_symlinks(device, superblock, parent_path)?;

    let allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;
    validate_create_destination(parent, name, &inodes, &entries)?;
    let inode_id = next_inode_id(&inodes)?;

    let new_entry = PersistedDirectoryEntry {
        parent,
        target: inode_id,
        name: name.to_owned(),
    };
    encode_directory_entry(&new_entry)?;

    inodes.push(PersistedInode {
        id: inode_id,
        kind,
        blocks: Vec::new(),
    });
    entries.push(new_entry);

    let report =
        store_create_metadata_journaled(device, superblock, &allocator, &inodes, &entries)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destination_split_preserves_root_and_nested_parent_paths() {
        assert_eq!(split_destination("/file").unwrap(), ("/", "file"));
        assert_eq!(
            split_destination("/dir/sub/file").unwrap(),
            ("/dir/sub", "file")
        );
    }

    #[test]
    fn destination_split_rejects_non_absolute_root_and_trailing_slash() {
        for path in ["file", "/", "/dir/"] {
            assert_eq!(
                split_destination(path).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
