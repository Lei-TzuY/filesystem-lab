use std::io;

use crate::block::BlockDevice;
use crate::format::{read_superblock, Superblock};
use crate::fsck::check_device;
use crate::journal::JournalEntry;
use crate::journal_region::{
    load_journal_image, load_retained_journal_entries, replace_retained_journal_entries,
    store_empty_journal_anchor,
};
use crate::recovery::{plan_recovery, recover_journal, RecoveryReport};
use crate::recovery_projection::{
    check_device_after_entries_projection, check_device_after_recovery_projection,
};

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetainedPrefixCheckpointReport {
    pub replayed: RecoveryReport,
    pub remaining_transactions: usize,
}

/// Replays and durably checkpoints a filesystem-consistent prefix of a retained v3 journal.
///
/// The selected prefix is semantically projected through strict fsck before any mutation. Once the
/// prefix is accepted, its home writes are replayed and flushed. The remaining complete transactions
/// are then published as a replacement v3 snapshot through the inactive bank, or the journal is
/// collapsed to the canonical v2 empty anchor when no suffix remains.
///
/// A crash before the replacement anchor becomes durable leaves the previous complete retained
/// snapshot authoritative, so replaying the prefix again is idempotent. A crash after the replacement
/// anchor becomes durable can expose only the retained suffix, and by then all prefix home writes have
/// crossed their durability boundary.
///
/// # Errors
///
/// Returns `InvalidInput` when `max_transactions` is zero or the current journal is not an active
/// v3 retained snapshot. Returns `InvalidData` for malformed/incomplete retained transactions or
/// when the selected prefix would expose a filesystem state rejected by strict fsck. Home-write,
/// flush, retained-snapshot replacement, and block-device errors are propagated.
pub fn recover_retained_prefix_and_checkpoint(
    device: &mut impl BlockDevice,
    superblock: Superblock,
    max_transactions: usize,
) -> io::Result<RetainedPrefixCheckpointReport> {
    if max_transactions == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "retained prefix checkpoint requires at least one transaction",
        ));
    }

    let entries = load_retained_journal_entries(device, superblock)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "retained prefix checkpoint requires an active v3 journal",
        )
    })?;
    let split = retained_prefix_split(&entries, max_transactions)?;
    let prefix = &entries[..split];
    let suffix = &entries[split..];

    let projected = check_device_after_entries_projection(device, prefix)?;
    let plan = plan_recovery(prefix)?;
    if projected.recovery != plan.report() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained prefix projection disagrees with recovery plan",
        ));
    }

    for (block, data) in plan.writes() {
        device.write_block(*block, data)?;
    }
    let replayed = plan.report();
    if replayed.home_writes != 0 {
        device.flush()?;
    }

    replace_retained_journal_entries(device, superblock, suffix)?;
    let remaining_transactions = plan_recovery(suffix)?.report().committed_transactions;

    Ok(RetainedPrefixCheckpointReport {
        replayed,
        remaining_transactions,
    })
}

fn retained_prefix_split(entries: &[JournalEntry], max_transactions: usize) -> io::Result<usize> {
    let mut active = None;
    let mut committed = 0_usize;

    for (index, entry) in entries.iter().enumerate() {
        match entry {
            JournalEntry::Begin { txid } => {
                if active.is_some() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "nested retained transaction while selecting checkpoint prefix",
                    ));
                }
                active = Some(*txid);
            }
            JournalEntry::Write { txid, .. } => {
                if active != Some(*txid) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "retained write does not match active checkpoint transaction",
                    ));
                }
            }
            JournalEntry::Commit { txid } => {
                if active != Some(*txid) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "retained commit does not match active checkpoint transaction",
                    ));
                }
                active = None;
                committed = committed.checked_add(1).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "retained checkpoint transaction count overflow",
                    )
                })?;
                if committed == max_transactions {
                    return Ok(index + 1);
                }
            }
        }
    }

    if active.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained journal ends with an incomplete transaction",
        ));
    }
    if committed == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retained journal contains no committed transaction",
        ));
    }
    Ok(entries.len())
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
