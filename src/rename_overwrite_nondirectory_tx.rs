use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_codec::{encode_directory_entry, PersistedDirectoryEntry};
use crate::directory_table::load_directory_table;
use crate::directory_tx::store_directory_table_journaled;
use crate::format::Superblock;
use crate::fsck::check_device;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
use crate::recovery::RecoveryReport;

/// Replaces one existing non-directory destination with another non-directory source.
///
/// Regular files and symbolic links may replace each other. If the destination has another
/// namespace reference, only the selected directory entry is replaced; otherwise the destination
/// inode and every block it owns are released atomically with namespace publication. Callers must
/// establish the recovery boundary before endpoint selection.
pub(crate) fn rename_overwrite_nondirectory_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    old_parent: u64,
    old_name: &str,
    new_parent: u64,
    new_name: &str,
) -> io::Result<RecoveryReport> {
    check_device(device)?;

    let mut allocator = load_allocator(device, superblock)?;
    let mut inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;

    validate_parent(&inodes, old_parent, "rename source parent")?;
    validate_parent(&inodes, new_parent, "rename destination parent")?;

    let source_index = entries
        .iter()
        .position(|entry| entry.parent == old_parent && entry.name == old_name)
        .ok_or_else(|| invalid_input("rename source entry does not exist"))?;
    let destination_index = entries
        .iter()
        .position(|entry| entry.parent == new_parent && entry.name == new_name)
        .ok_or_else(|| invalid_input("rename destination entry does not exist"))?;
    if source_index == destination_index {
        return Ok(RecoveryReport::default());
    }

    let source_target = entries[source_index].target;
    let destination_target = entries[destination_index].target;
    if source_target == destination_target {
        return Err(invalid_input(
            "rename source and destination may not alias the same inode",
        ));
    }

    let source_inode = inodes
        .iter()
        .find(|inode| inode.id == source_target)
        .ok_or_else(|| invalid_input("rename source targets a missing inode"))?;
    let destination_inode = inodes
        .iter()
        .find(|inode| inode.id == destination_target)
        .ok_or_else(|| invalid_input("rename destination targets a missing inode"))?;
    if source_inode.kind == InodeKind::Directory || destination_inode.kind == InodeKind::Directory {
        return Err(invalid_input(
            "non-directory rename overwrite cannot use directory endpoints",
        ));
    }
    let destination_blocks = destination_inode.blocks.clone();
    let destination_references = entries
        .iter()
        .filter(|entry| entry.target == destination_target)
        .count();
    if destination_references == 0 {
        return Err(invalid_input(
            "rename destination has no namespace reference",
        ));
    }

    let replacement = PersistedDirectoryEntry {
        parent: new_parent,
        target: source_target,
        name: new_name.to_owned(),
    };
    encode_directory_entry(&replacement)?;

    let mut desired_entries = Vec::with_capacity(entries.len() - 1);
    for (index, entry) in entries.into_iter().enumerate() {
        if index == destination_index {
            continue;
        }
        if index == source_index {
            desired_entries.push(replacement.clone());
        } else {
            desired_entries.push(entry);
        }
    }

    if destination_references > 1 {
        let report = store_directory_table_journaled(device, superblock, &desired_entries)?;
        recover_journal_and_checkpoint(device, *superblock)?;
        return Ok(report);
    }

    for block in destination_blocks {
        allocator.free(block).map_err(|error| {
            invalid_input(format!("rename destination block release failed: {error}"))
        })?;
    }
    inodes.retain(|inode| inode.id != destination_target);

    store_create_metadata_journaled(device, superblock, &allocator, &inodes, &desired_entries)
}

fn validate_parent(
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

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
