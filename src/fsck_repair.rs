use std::collections::BTreeSet;
use std::io;

use crate::allocation_disk::load_allocator;
use crate::allocation_tx::store_allocator_journaled;
use crate::block::BlockDevice;
use crate::directory_codec::PersistedDirectoryEntry;
use crate::directory_table::load_directory_table;
use crate::directory_tx::store_directory_table_journaled;
use crate::format::{read_superblock, Superblock};
use crate::fsck::{
    check_device, check_device_allowing_orphaned_allocations,
    check_device_allowing_unreachable_inodes, validate_namespace_snapshot, ROOT_INODE_ID,
};
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::{checkpoint_journal, recover_journal_and_checkpoint};
use crate::recovery::RecoveryReport;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceReattachment {
    pub inode_id: u64,
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrphanNamespaceRepairReport {
    pub reattached: Vec<NamespaceReattachment>,
    pub prior_recovery: RecoveryReport,
    pub repair_transaction: RecoveryReport,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrphanAllocationRepairReport {
    pub released_blocks: Vec<u64>,
    pub prior_recovery: RecoveryReport,
    pub repair_transaction: RecoveryReport,
}

/// Reattaches namespace-orphaned inode components beneath the root directory.
///
/// The repair tolerates only root reachability failure. Allocation ownership, inode payloads,
/// directory endpoint validity, directory cycles, journal integrity, and every other fsck invariant
/// must already be valid. Each top-level unreachable component receives one deterministic
/// collision-safe root entry named `.fsck-orphan-<inode>` (with a numeric suffix only when needed).
///
/// Existing orphan subtrees are preserved intact: child entries are not flattened and inode/data
/// contents are not rewritten. The complete desired directory snapshot is validated strictly before
/// publication, then persisted through one bounded directory-table WAL transaction. Final strict
/// fsck must accept the repaired filesystem before success is reported.
///
/// # Errors
///
/// Returns `InvalidInput` when the supplied superblock does not match block zero or the bounded
/// journal cannot hold the directory repair. Returns `InvalidData` for any corruption other than
/// namespace unreachability, for an orphan topology with no repairable component root, or for a
/// desired repaired namespace that would violate strict namespace invariants.
pub fn repair_unreachable_inodes_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
) -> io::Result<OrphanNamespaceRepairReport> {
    let durable_superblock = read_superblock(device)?;
    if durable_superblock != *superblock {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "repair superblock does not match durable filesystem superblock",
        ));
    }

    let prior_recovery = recover_journal_and_checkpoint(device, *superblock)?;
    let (_, unreachable) = check_device_allowing_unreachable_inodes(device)?;
    if unreachable.is_empty() {
        return Ok(OrphanNamespaceRepairReport {
            reattached: Vec::new(),
            prior_recovery,
            repair_transaction: RecoveryReport::default(),
        });
    }

    let inodes = load_inode_table(device, superblock)?;
    let mut entries = load_directory_table(device, superblock)?;
    let unreachable_set = unreachable.iter().copied().collect::<BTreeSet<_>>();

    let component_roots = unreachable
        .iter()
        .copied()
        .filter(|inode_id| {
            !entries
                .iter()
                .any(|entry| unreachable_set.contains(&entry.parent) && entry.target == *inode_id)
        })
        .collect::<Vec<_>>();
    if component_roots.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unreachable namespace has no acyclic component root",
        ));
    }

    let mut occupied = entries
        .iter()
        .filter(|entry| entry.parent == ROOT_INODE_ID)
        .map(|entry| entry.name.clone())
        .collect::<BTreeSet<_>>();
    let mut reattached = Vec::with_capacity(component_roots.len());

    for inode_id in component_roots {
        let base = format!(".fsck-orphan-{inode_id}");
        let mut name = base.clone();
        let mut suffix = 1_u64;
        while occupied.contains(&name) {
            name = format!("{base}-{suffix}");
            suffix = suffix.checked_add(1).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "orphan repair name suffix overflow",
                )
            })?;
        }
        occupied.insert(name.clone());
        entries.push(PersistedDirectoryEntry {
            parent: ROOT_INODE_ID,
            target: inode_id,
            name: name.clone(),
        });
        reattached.push(NamespaceReattachment { inode_id, name });
    }

    validate_namespace_snapshot(&inodes, &entries)?;
    let repair_transaction = store_directory_table_journaled(device, superblock, &entries)?;
    if repair_transaction.committed_transactions != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "orphan namespace repair did not publish exactly one committed transaction",
        ));
    }
    if !checkpoint_journal(device, *superblock)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "orphan namespace repair transaction was not present for checkpoint",
        ));
    }

    check_device(device)?;

    Ok(OrphanNamespaceRepairReport {
        reattached,
        prior_recovery,
        repair_transaction,
    })
}

/// Releases durable data-block allocations that have no persisted inode owner.
///
/// The operation is intentionally narrow. It first verifies that the caller's superblock matches
/// block zero, recovers and checkpoints any older WAL state, then performs every fsck check except
/// the orphan-allocation invariant. Any unrelated corruption is rejected before a repair
/// transaction is published.
///
/// When orphaned allocations exist, exactly those blocks are released from the allocator through the
/// existing bounded journaled allocator transaction. The committed allocator image is made durable,
/// the journal is checkpointed, and strict read-only fsck must accept the resulting filesystem before
/// success is reported. Referenced blocks, inode records, namespace entries, and file data are never
/// rewritten by this repair.
///
/// # Errors
///
/// Returns `InvalidInput` when the supplied superblock does not match the durable superblock or when
/// the bounded journal cannot contain the repaired allocator image. Returns `InvalidData` for any
/// corruption other than unreferenced allocated data blocks, allocator/freeing disagreement,
/// an inconsistent repair transaction/checkpoint result, or a post-repair fsck failure. Recovery,
/// journal, checkpoint, and block-device I/O failures are propagated.
pub fn repair_orphaned_allocations_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
) -> io::Result<OrphanAllocationRepairReport> {
    let durable_superblock = read_superblock(device)?;
    if durable_superblock != *superblock {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "repair superblock does not match durable filesystem superblock",
        ));
    }

    let prior_recovery = recover_journal_and_checkpoint(device, *superblock)?;
    let (_, orphaned_allocations) = check_device_allowing_orphaned_allocations(device)?;

    if orphaned_allocations.is_empty() {
        return Ok(OrphanAllocationRepairReport {
            released_blocks: Vec::new(),
            prior_recovery,
            repair_transaction: RecoveryReport::default(),
        });
    }

    let mut allocator = load_allocator(device, superblock)?;
    for block in &orphaned_allocations {
        allocator
            .free(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    }

    let repair_transaction = store_allocator_journaled(device, superblock, &allocator)?;
    if repair_transaction.committed_transactions != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "orphan-allocation repair did not publish exactly one committed transaction",
        ));
    }
    if !checkpoint_journal(device, *superblock)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "orphan-allocation repair transaction was not present for checkpoint",
        ));
    }

    check_device(device)?;

    Ok(OrphanAllocationRepairReport {
        released_blocks: orphaned_allocations,
        prior_recovery,
        repair_transaction,
    })
}
