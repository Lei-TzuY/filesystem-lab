use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io;

use crate::allocation::BlockAllocator;
use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::create_tx::store_create_metadata_journaled;
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::fsck::{check_device, validate_namespace_snapshot, ROOT_INODE_ID};
use crate::inode::InodeKind;
use crate::inode_codec::PersistedInode;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint_checked;
use crate::recovery::RecoveryReport;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecursiveRemoveReport {
    pub removed_entries: usize,
    pub removed_inodes: Vec<u64>,
    pub released_blocks: Vec<u64>,
    pub transaction: RecoveryReport,
}

struct RecursiveRemovePlan {
    allocator: BlockAllocator,
    desired_inodes: Vec<PersistedInode>,
    desired_entries: Vec<PersistedDirectoryEntry>,
    removed_entries: usize,
    removed_inodes: Vec<u64>,
    released_blocks: Vec<u64>,
}

/// Atomically removes one complete directory subtree from a clean recovered filesystem.
///
/// The selected target must be a non-root directory. Every directory in the removed subtree must
/// have no namespace reference from outside the removed entry set; this keeps directory-parent
/// semantics fail-closed even though strict fsck currently permits multiple directory links.
///
/// Regular files and symbolic links are reference-aware: namespace entries inside the subtree are
/// removed, but an inode and its blocks are retained when another namespace entry outside the
/// subtree still targets it. An inode is retired and its blocks released only when the recursive
/// removal eliminates its final durable namespace reference.
///
/// The complete desired allocation, inode, and directory snapshots are published through one bounded
/// WAL transaction. Existing committed WAL is checked/recovered/checkpointed first, the starting
/// filesystem must pass strict fsck, the desired namespace is validated before publication, and
/// strict fsck must accept the committed result.
///
/// # Errors
///
/// Returns `InvalidInput` for a missing/non-directory/root target or when a subtree directory has an
/// external namespace reference. Returns `InvalidData` for any pre-existing fsck corruption,
/// allocator ownership disagreement, an inconsistent desired namespace, or a post-commit fsck
/// failure. Journal-capacity, recovery, checkpoint, and device I/O failures are propagated.
pub fn remove_directory_tree_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    parent: u64,
    name: &str,
) -> io::Result<RecursiveRemoveReport> {
    recover_journal_and_checkpoint_checked(device, *superblock)?;
    check_device(device)?;

    let allocator = load_allocator(device, superblock)?;
    let inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;
    let plan = build_recursive_remove_plan(allocator, inodes, entries, parent, name)?;
    publish_recursive_remove(device, superblock, plan)
}

fn build_recursive_remove_plan(
    mut allocator: BlockAllocator,
    inodes: Vec<PersistedInode>,
    entries: Vec<PersistedDirectoryEntry>,
    parent: u64,
    name: &str,
) -> io::Result<RecursiveRemovePlan> {
    let inode_kinds = inodes
        .iter()
        .map(|inode| (inode.id, inode.kind))
        .collect::<BTreeMap<_, _>>();

    let selected_index = entries
        .iter()
        .position(|entry| entry.parent == parent && entry.name == name)
        .ok_or_else(|| invalid_input("recursive-remove directory entry is missing"))?;
    let target = entries[selected_index].target;
    if target == ROOT_INODE_ID {
        return Err(invalid_input(
            "recursive-remove cannot remove the root inode",
        ));
    }
    if inode_kinds.get(&target) != Some(&InodeKind::Directory) {
        return Err(invalid_input(
            "recursive-remove target must be a directory inode",
        ));
    }

    let subtree_directories = collect_subtree_directories(target, &inode_kinds, &entries);
    let removed_entry_indices =
        collect_removed_entry_indices(selected_index, &subtree_directories, &entries);
    reject_external_directory_references(&subtree_directories, &removed_entry_indices, &entries)?;

    let desired_entries = entries
        .iter()
        .enumerate()
        .filter(|(index, _)| !removed_entry_indices.contains(index))
        .map(|(_, entry)| entry.clone())
        .collect::<Vec<_>>();

    let removed_inodes = collect_removed_inodes(
        &subtree_directories,
        &removed_entry_indices,
        &entries,
        &desired_entries,
    );
    let removed_set = removed_inodes.iter().copied().collect::<BTreeSet<_>>();
    let desired_inodes = inodes
        .iter()
        .filter(|inode| !removed_set.contains(&inode.id))
        .cloned()
        .collect::<Vec<_>>();
    let released_blocks = release_removed_inode_blocks(&mut allocator, &inodes, &removed_set)?;

    Ok(RecursiveRemovePlan {
        allocator,
        desired_inodes,
        desired_entries,
        removed_entries: removed_entry_indices.len(),
        removed_inodes,
        released_blocks,
    })
}

fn collect_removed_entry_indices(
    selected_index: usize,
    subtree_directories: &BTreeSet<u64>,
    entries: &[PersistedDirectoryEntry],
) -> BTreeSet<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            if index == selected_index || subtree_directories.contains(&entry.parent) {
                Some(index)
            } else {
                None
            }
        })
        .collect()
}

fn reject_external_directory_references(
    subtree_directories: &BTreeSet<u64>,
    removed_entry_indices: &BTreeSet<usize>,
    entries: &[PersistedDirectoryEntry],
) -> io::Result<()> {
    let has_external_reference = subtree_directories.iter().any(|directory| {
        entries.iter().enumerate().any(|(index, entry)| {
            entry.target == *directory && !removed_entry_indices.contains(&index)
        })
    });
    if has_external_reference {
        return Err(invalid_input(
            "recursive-remove subtree directory has an external namespace reference",
        ));
    }
    Ok(())
}

fn collect_removed_inodes(
    subtree_directories: &BTreeSet<u64>,
    removed_entry_indices: &BTreeSet<usize>,
    entries: &[PersistedDirectoryEntry],
    desired_entries: &[PersistedDirectoryEntry],
) -> Vec<u64> {
    let mut candidate_targets = removed_entry_indices
        .iter()
        .map(|index| entries[*index].target)
        .collect::<BTreeSet<_>>();
    candidate_targets.extend(subtree_directories.iter().copied());

    let mut removed_inodes = candidate_targets
        .into_iter()
        .filter(|inode_id| {
            subtree_directories.contains(inode_id)
                || !desired_entries
                    .iter()
                    .any(|entry| entry.target == *inode_id)
        })
        .collect::<Vec<_>>();
    removed_inodes.sort_unstable();
    removed_inodes
}

fn release_removed_inode_blocks(
    allocator: &mut BlockAllocator,
    inodes: &[PersistedInode],
    removed_set: &BTreeSet<u64>,
) -> io::Result<Vec<u64>> {
    let mut released_blocks = Vec::new();
    for inode in inodes
        .iter()
        .filter(|inode| removed_set.contains(&inode.id))
    {
        for block in &inode.blocks {
            if !allocator
                .is_owned(*block)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
            {
                return Err(invalid_data(
                    "recursive-remove inode block is not allocator-owned",
                ));
            }
            allocator
                .free(*block)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            released_blocks.push(*block);
        }
    }
    released_blocks.sort_unstable();
    Ok(released_blocks)
}

fn publish_recursive_remove(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    plan: RecursiveRemovePlan,
) -> io::Result<RecursiveRemoveReport> {
    validate_namespace_snapshot(&plan.desired_inodes, &plan.desired_entries)?;

    let transaction = store_create_metadata_journaled(
        device,
        superblock,
        &plan.allocator,
        &plan.desired_inodes,
        &plan.desired_entries,
    )?;
    if transaction.committed_transactions != 1 {
        return Err(invalid_data(
            "recursive-remove did not publish exactly one committed transaction",
        ));
    }

    check_device(device)?;

    Ok(RecursiveRemoveReport {
        removed_entries: plan.removed_entries,
        removed_inodes: plan.removed_inodes,
        released_blocks: plan.released_blocks,
        transaction,
    })
}

fn collect_subtree_directories(
    target: u64,
    inode_kinds: &BTreeMap<u64, InodeKind>,
    entries: &[PersistedDirectoryEntry],
) -> BTreeSet<u64> {
    let mut directories = BTreeSet::from([target]);
    let mut pending = VecDeque::from([target]);

    while let Some(parent) = pending.pop_front() {
        for entry in entries.iter().filter(|entry| entry.parent == parent) {
            if inode_kinds.get(&entry.target) == Some(&InodeKind::Directory)
                && directories.insert(entry.target)
            {
                pending.push_back(entry.target);
            }
        }
    }

    directories
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
