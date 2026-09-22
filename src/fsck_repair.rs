use std::io;

use crate::allocation_disk::load_allocator;
use crate::allocation_tx::store_allocator_journaled;
use crate::block::BlockDevice;
use crate::format::{read_superblock, Superblock};
use crate::fsck::{check_device, check_device_allowing_orphaned_allocations};
use crate::journal_checkpoint::{checkpoint_journal, recover_journal_and_checkpoint};
use crate::recovery::RecoveryReport;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OrphanAllocationRepairReport {
    pub released_blocks: Vec<u64>,
    pub prior_recovery: RecoveryReport,
    pub repair_transaction: RecoveryReport,
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
