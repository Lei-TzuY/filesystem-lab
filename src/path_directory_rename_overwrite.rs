use std::collections::{BTreeMap, BTreeSet};
use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_codec::{encode_directory_entry, PersistedDirectoryEntry};
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::fsck::check_device;
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::load_inode_table;
use crate::path_lookup::resolve_path_following_symlinks;
use crate::recovery::RecoveryReport;

pub fn rename_overwrite_directory_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    source: &str,
    destination: &str,
) -> io::Result<RecoveryReport> {
    let (old_parent_path, old_name) = split_path(source, "directory rename-overwrite source")?;
    let (new_parent_path, new_name) =
        split_path(destination, "directory rename-overwrite destination")?;
    let old_parent = resolve_path_following_symlinks(device, superblock, old_parent_path)?;
    let new_parent = resolve_path_following_symlinks(device, superblock, new_parent_path)?;
    rename_overwrite_directory_journaled(
        device, superblock, old_parent, old_name, new_parent, new_name,
    )
}

/// Atomically moves a directory over an existing empty directory.
///
/// The destination must be singly referenced and empty. Its inode and any owned blocks are released
/// in the same allocation+inode+directory WAL transaction that publishes the source under the
/// destination name. The complete candidate namespace is cycle-checked before publication.
pub fn rename_overwrite_directory_journaled(
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
    validate_directory(&inodes, old_parent, "source parent")?;
    validate_directory(&inodes, new_parent, "destination parent")?;

    let source_index = find_entry(&entries, old_parent, old_name, "source")?;
    let destination_index = find_entry(&entries, new_parent, new_name, "destination")?;
    if source_index == destination_index {
        return Ok(RecoveryReport::default());
    }
    let source_target = entries[source_index].target;
    let destination_target = entries[destination_index].target;
    if source_target == destination_target {
        return Err(invalid_input(
            "directory rename-overwrite endpoints alias the same inode",
        ));
    }
    validate_directory(&inodes, source_target, "source")?;
    validate_directory(&inodes, destination_target, "destination")?;
    if source_target == 1 || destination_target == 1 {
        return Err(invalid_input(
            "directory rename-overwrite cannot replace the root inode",
        ));
    }
    if entries
        .iter()
        .filter(|entry| entry.target == destination_target)
        .count()
        != 1
    {
        return Err(invalid_input(
            "directory rename-overwrite destination must be singly referenced",
        ));
    }
    if entries
        .iter()
        .any(|entry| entry.parent == destination_target)
    {
        return Err(invalid_input(
            "directory rename-overwrite destination must be empty",
        ));
    }

    let destination_blocks = inodes
        .iter()
        .find(|inode| inode.id == destination_target)
        .expect("validated destination inode")
        .blocks
        .clone();
    let unique_blocks: BTreeSet<u64> = destination_blocks.iter().copied().collect();
    if unique_blocks.len() != destination_blocks.len() {
        return Err(invalid_input(
            "destination directory contains duplicate block references",
        ));
    }
    for block in &destination_blocks {
        if !allocator
            .is_owned(*block)
            .map_err(|error| invalid_input(error.to_string()))?
        {
            return Err(invalid_input(
                "destination directory block is not allocator-owned",
            ));
        }
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
    validate_acyclic(&desired_entries, &inodes)?;

    for block in destination_blocks {
        allocator
            .free(block)
            .map_err(|error| invalid_input(error.to_string()))?;
    }
    inodes.retain(|inode| inode.id != destination_target);
    store_create_metadata_journaled(device, superblock, &allocator, &inodes, &desired_entries)
}

fn validate_acyclic(
    entries: &[PersistedDirectoryEntry],
    inodes: &[PersistedInode],
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
                return Err(invalid_input(
                    "directory rename-overwrite would create a cycle",
                ));
            }
            current = parent;
        }
    }
    Ok(())
}

fn find_entry(
    entries: &[PersistedDirectoryEntry],
    parent: u64,
    name: &str,
    label: &str,
) -> io::Result<usize> {
    entries
        .iter()
        .position(|entry| entry.parent == parent && entry.name == name)
        .ok_or_else(|| {
            invalid_input(format!(
                "directory rename-overwrite {label} entry does not exist"
            ))
        })
}

fn validate_directory(inodes: &[PersistedInode], id: u64, label: &str) -> io::Result<()> {
    let inode = inodes.iter().find(|inode| inode.id == id).ok_or_else(|| {
        invalid_input(format!(
            "directory rename-overwrite {label} inode does not exist"
        ))
    })?;
    if inode.kind != InodeKind::Directory {
        return Err(invalid_input(format!(
            "directory rename-overwrite {label} must be a directory"
        )));
    }
    Ok(())
}

fn split_path<'a>(path: &'a str, label: &str) -> io::Result<(&'a str, &'a str)> {
    if !path.starts_with('/') || path == "/" {
        return Err(invalid_input(format!(
            "{label} must name a non-root absolute path"
        )));
    }
    let (parent, name) = path
        .rsplit_once('/')
        .ok_or_else(|| invalid_input(format!("{label} lacks a final component")))?;
    if name.is_empty() {
        return Err(invalid_input(format!("{label} final component is empty")));
    }
    Ok((if parent.is_empty() { "/" } else { parent }, name))
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}
