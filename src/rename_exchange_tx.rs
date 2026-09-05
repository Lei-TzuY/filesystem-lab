use std::io;

use crate::block::BlockDevice;
use crate::directory_codec::{encode_directory_entry, PersistedDirectoryEntry};
use crate::directory_table::load_directory_table;
use crate::directory_tx::store_directory_table_journaled;
use crate::format::Superblock;
use crate::fsck::check_device;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::recovery::RecoveryReport;

/// Atomically exchanges two existing regular-file namespace entries.
///
/// The two directory keys stay in place while their target inode identifiers are swapped in one
/// directory-table WAL transaction. Both file inodes, their data blocks, and allocator ownership
/// remain unchanged. This bounded format-v5 operation deliberately excludes directories and does
/// not add persisted link counts or other rename flags.
///
/// Exchanging two hard-link aliases of the same inode, or exchanging a path with itself, is a
/// durable no-op and does not publish a journal transaction.
///
/// # Errors
///
/// Returns `InvalidInput` when either parent is missing or is not a directory, either namespace
/// entry is missing, either target is missing or is not a regular file, or either persisted entry
/// cannot be encoded. Existing corruption is rejected by fsck before WAL publication. Journal,
/// recovery, checkpoint, and block-device failures are propagated.
pub fn rename_exchange_files_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first_parent: u64,
    first_name: &str,
    second_parent: u64,
    second_name: &str,
) -> io::Result<RecoveryReport> {
    check_device(device)?;

    let inodes = load_inode_table(device, superblock)?;
    validate_directory_parent(&inodes, first_parent, "first exchange parent")?;
    validate_directory_parent(&inodes, second_parent, "second exchange parent")?;

    if first_parent == second_parent && first_name == second_name {
        return Ok(RecoveryReport::default());
    }

    let mut entries = load_directory_table(device, superblock)?;
    let first_index = entries
        .iter()
        .position(|entry| entry.parent == first_parent && entry.name == first_name)
        .ok_or_else(|| invalid_input("first exchange entry does not exist"))?;
    let second_index = entries
        .iter()
        .position(|entry| entry.parent == second_parent && entry.name == second_name)
        .ok_or_else(|| invalid_input("second exchange entry does not exist"))?;

    let first_target = entries[first_index].target;
    let second_target = entries[second_index].target;
    validate_regular_file_target(&inodes, first_target, "first exchange target")?;
    validate_regular_file_target(&inodes, second_target, "second exchange target")?;

    if first_target == second_target {
        return Ok(RecoveryReport::default());
    }

    let first_replacement = PersistedDirectoryEntry {
        parent: first_parent,
        target: second_target,
        name: first_name.to_owned(),
    };
    let second_replacement = PersistedDirectoryEntry {
        parent: second_parent,
        target: first_target,
        name: second_name.to_owned(),
    };
    encode_directory_entry(&first_replacement)?;
    encode_directory_entry(&second_replacement)?;

    entries[first_index] = first_replacement;
    entries[second_index] = second_replacement;
    store_directory_table_journaled(device, superblock, &entries)
}

fn validate_directory_parent(
    inodes: &[crate::inode_codec::PersistedInode],
    parent: u64,
    label: &str,
) -> io::Result<()> {
    let inode = inodes
        .iter()
        .find(|inode| inode.id == parent)
        .ok_or_else(|| invalid_input(format!("{label} inode does not exist")))?;
    if inode.kind != InodeKind::Directory {
        return Err(invalid_input(format!("{label} inode is not a directory")));
    }
    Ok(())
}

fn validate_regular_file_target(
    inodes: &[crate::inode_codec::PersistedInode],
    target: u64,
    label: &str,
) -> io::Result<()> {
    let inode = inodes
        .iter()
        .find(|inode| inode.id == target)
        .ok_or_else(|| invalid_input(format!("{label} inode does not exist")))?;
    if inode.kind != InodeKind::File {
        return Err(invalid_input(format!("{label} inode is not a regular file")));
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
