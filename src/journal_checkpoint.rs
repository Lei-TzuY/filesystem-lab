use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::Superblock;
use crate::journal_region::load_journal_image;
use crate::recovery::{recover_journal, RecoveryReport};

/// Clears a fully processed persistent journal after validating its current image.
///
/// The checkpoint uses the filesystem's existing flush durability model: every journal block is
/// overwritten with zeroes, then one `flush` makes the empty reservation durable. A crash before
/// that flush leaves the previous durable journal image intact, so recovery can replay it again.
/// A crash after the flush exposes a completely empty journal. This intentionally does not model
/// sector tearing or controller reordering beyond the `BlockDevice` contract.
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

    let zero = [0_u8; BLOCK_SIZE];
    for offset in 0..superblock.journal_blocks {
        let block = superblock
            .journal_start
            .checked_add(offset)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "journal block index overflow"))?;
        device.write_block(block, &zero)?;
    }
    device.flush()?;
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
