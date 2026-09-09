use std::collections::{BTreeMap, BTreeSet};
use std::io;

use crate::block::BlockDevice;
use crate::directory_codec::{encode_directory_entry, PersistedDirectoryEntry};
use crate::directory_table::load_directory_table;
use crate::directory_tx::store_directory_table_journaled;
use crate::format::Superblock;
use crate::fsck::check_device;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::recovery::RecoveryReport;

#[derive(Clone, Copy)]
struct ExchangeTargetPolicy {
    kind: InodeKind,
    label: &'static str,
    reject_directory_cycles: bool,
}

const FILE_POLICY: ExchangeTargetPolicy = ExchangeTargetPolicy {
    kind: InodeKind::File,
    label: "regular file",
    reject_directory_cycles: false,
};
const SYMLINK_POLICY: ExchangeTargetPolicy = ExchangeTargetPolicy {
    kind: InodeKind::Symlink,
    label: "symbolic link",
    reject_directory_cycles: false,
};
const DIRECTORY_POLICY: ExchangeTargetPolicy = ExchangeTargetPolicy {
    kind: InodeKind::Directory,
    label: "directory",
    reject_directory_cycles: true,
};

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
    rename_exchange_by_kind_journaled(
        device,
        superblock,
        first_parent,
        first_name,
        second_parent,
        second_name,
        FILE_POLICY,
    )
}

/// Atomically exchanges two existing symbolic-link namespace entries.
///
/// Final link inodes are exchanged exactly as named. Their persisted target payload blocks and
/// allocator ownership remain unchanged because only the directory table is published through the
/// WAL. Exchanging aliases of the same symlink inode, or a path with itself, is a durable no-op.
///
/// # Errors
///
/// Returns `InvalidInput` when either parent is missing or is not a directory, either namespace
/// entry is missing, either target is missing or is not a symbolic link, or either persisted entry
/// cannot be encoded. Existing corruption is rejected by fsck before WAL publication. Journal,
/// recovery, checkpoint, and block-device failures are propagated.
pub fn rename_exchange_symlinks_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first_parent: u64,
    first_name: &str,
    second_parent: u64,
    second_name: &str,
) -> io::Result<RecoveryReport> {
    rename_exchange_by_kind_journaled(
        device,
        superblock,
        first_parent,
        first_name,
        second_parent,
        second_name,
        SYMLINK_POLICY,
    )
}

/// Atomically exchanges two existing directory namespace entries.
///
/// Only the directory-table targets are swapped; inode and allocator images remain unchanged. The
/// complete candidate namespace is checked for directory cycles before WAL publication, so an
/// exchange that would place an ancestor below its descendant is rejected without durable writes.
///
/// # Errors
///
/// Returns `InvalidInput` when either parent is missing or is not a directory, either namespace
/// entry is missing, either target is missing or is not a directory, either persisted entry cannot
/// be encoded, or the exchanged namespace would contain a directory cycle. Existing corruption is
/// rejected by fsck before WAL publication. Journal, recovery, checkpoint, and block-device
/// failures are propagated.
pub fn rename_exchange_directories_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first_parent: u64,
    first_name: &str,
    second_parent: u64,
    second_name: &str,
) -> io::Result<RecoveryReport> {
    rename_exchange_by_kind_journaled(
        device,
        superblock,
        first_parent,
        first_name,
        second_parent,
        second_name,
        DIRECTORY_POLICY,
    )
}

fn rename_exchange_by_kind_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    first_parent: u64,
    first_name: &str,
    second_parent: u64,
    second_name: &str,
    policy: ExchangeTargetPolicy,
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
    validate_target_kind(&inodes, first_target, policy, "first exchange target")?;
    validate_target_kind(&inodes, second_target, policy, "second exchange target")?;

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
    if policy.reject_directory_cycles {
        validate_directory_acyclic(&entries, &inodes)?;
    }

    let report = store_directory_table_journaled(device, superblock, &entries)?;
    recover_journal_and_checkpoint(device, *superblock)?;
    Ok(report)
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

fn validate_target_kind(
    inodes: &[crate::inode_codec::PersistedInode],
    target: u64,
    policy: ExchangeTargetPolicy,
    label: &str,
) -> io::Result<()> {
    let inode = inodes
        .iter()
        .find(|inode| inode.id == target)
        .ok_or_else(|| invalid_input(format!("{label} inode does not exist")))?;
    if inode.kind != policy.kind {
        return Err(invalid_input(format!(
            "{label} inode is not a {}",
            policy.label
        )));
    }
    Ok(())
}

fn validate_directory_acyclic(
    entries: &[PersistedDirectoryEntry],
    inodes: &[crate::inode_codec::PersistedInode],
) -> io::Result<()> {
    let directories: BTreeSet<u64> = inodes
        .iter()
        .filter(|inode| inode.kind == InodeKind::Directory)
        .map(|inode| inode.id)
        .collect();
    let mut parent_of = BTreeMap::new();
    for entry in entries {
        if directories.contains(&entry.target) {
            parent_of.insert(entry.target, entry.parent);
        }
    }

    for directory in directories {
        let mut seen = BTreeSet::new();
        let mut current = directory;
        while let Some(parent) = parent_of.get(&current).copied() {
            if !seen.insert(current) {
                return Err(invalid_input("directory exchange would create a cycle"));
            }
            current = parent;
        }
    }
    Ok(())
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
