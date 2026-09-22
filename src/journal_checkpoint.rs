use std::io;

use crate::block::BlockDevice;
use crate::format::{read_superblock, Superblock};
use crate::fsck::check_device;
use crate::journal_region::{load_journal_image, store_empty_journal_anchor};
use crate::recovery::{recover_journal, RecoveryReport};
use crate::recovery_projection::check_device_after_recovery_projection;

/// Clears a fully processed persistent journal after validating its current image.
///
/// The checkpoint publishes one checksummed version-2 empty anchor in the header-bearing journal
/// block after home replay has crossed its durability boundary. Later journal blocks are deliberately
/// left untouched and are non-authoritative while the empty anchor is present.
///
/// The `BlockDevice` contract allows an issued write to become durable before `flush`. Under the
/// whole-block persistence model, a crash therefore exposes either the previous complete active
/// anchor or the complete empty anchor; it cannot expose a partially zeroed multi-block journal that
/// fails to decode merely because some pre-flush writes reached storage early.
///
/// Returns `Ok(false)` when the journal is already empty and no writes or flush are needed.
///
/// # Errors
///
/// Returns an error if the current journal image is corrupt, the superblock/device geometry is
/// invalid, journal block arithmetic overflows, or an underlying write/flush fails.
pub fn checkpoint_journal(
    device: &mut impl BlockDevice,
    superblock: Superblock,
) -> io::Result<bool> {
    if load_journal_image(device, superblock)?.is_empty() {
        return Ok(false);
    }

    store_empty_journal_anchor(device, superblock)?;
    Ok(true)
}

/// Replays committed journal transactions to home locations and then checkpoints the journal.
///
/// `recover_journal` first establishes durability of all replayed home writes. Only after that
/// durability boundary succeeds does `checkpoint_journal` clear the persistent log. Therefore a
/// crash during checkpointing can never discard the only durable copy of a committed transaction.
///
/// # Errors
///
/// Propagates recovery, journal-validation, checkpoint write, and checkpoint flush failures.
pub fn recover_journal_and_checkpoint(
    device: &mut impl BlockDevice,
    superblock: Superblock,
) -> io::Result<RecoveryReport> {
    let report = recover_journal(device, superblock)?;
    checkpoint_journal(device, superblock)?;
    Ok(report)
}

/// Semantically validates committed WAL replay before mutating home locations, then recovers and
/// checkpoints the journal.
///
/// This higher-level boundary is for callers that require a complete filesystem state rather than a
/// low-level table transaction. It first verifies that the supplied superblock matches block zero,
/// then inspects the durable journal. An already empty journal is a no-op. Otherwise strict fsck
/// runs against the in-memory post-replay projection before any home write or checkpoint mutation.
///
/// Once a non-empty journal projection is valid, the ordinary recovery/checkpoint path performs the
/// durable replay.
/// The actual recovery report must match the preflight plan, and strict fsck must accept the final
/// checkpointed home state before success is reported.
///
/// # Errors
///
/// Returns `InvalidInput` when the supplied superblock does not match the durable superblock.
/// Propagates projected-fsck, recovery, checkpoint, and final-fsck errors. Returns `InvalidData` if
/// the actual replay report disagrees with the validated projection.
pub fn recover_journal_and_checkpoint_checked(
    device: &mut impl BlockDevice,
    superblock: Superblock,
) -> io::Result<RecoveryReport> {
    let durable_superblock = read_superblock(device)?;
    if durable_superblock != superblock {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "checked recovery superblock does not match durable filesystem superblock",
        ));
    }

    if load_journal_image(device, superblock)?.is_empty() {
        return Ok(RecoveryReport::default());
    }

    let projected = check_device_after_recovery_projection(device)?;
    let report = recover_journal_and_checkpoint(device, superblock)?;
    if report != projected.recovery {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "checked recovery report disagrees with projected replay",
        ));
    }
    check_device(device)?;
    Ok(report)
}
