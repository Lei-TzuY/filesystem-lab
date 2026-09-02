use std::io;

use crate::block::BlockDevice;
use crate::format::{read_superblock, Superblock};
use crate::journal::{JournalEntry, TransactionId};
use crate::journal_region::load_journal_image;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FsckReport {
    pub total_blocks: u64,
    pub reserved_blocks: u64,
    pub data_blocks: u64,
    pub journal_entries: usize,
    pub journal_writes: usize,
    pub committed_transactions: usize,
    pub pending_transaction: Option<TransactionId>,
}

/// Performs a read-only consistency check over the durable filesystem layers that currently exist.
///
/// The check validates the superblock against the opened device, validates and decodes the complete
/// bounded journal region, and independently audits journal transaction structure and home-block
/// ownership. It never writes or flushes the device.
///
/// An incomplete final transaction is reported as `pending_transaction` rather than treated as
/// corruption because it is the expected durable state after a crash before the commit marker.
///
/// # Errors
///
/// Returns `InvalidData` for malformed/corrupt durable metadata, including invalid superblock
/// geometry, journal checksum/record corruption, malformed transaction ordering, or journal writes
/// that target reserved/out-of-range blocks. Underlying device read errors are propagated.
pub fn check_device(device: &mut impl BlockDevice) -> io::Result<FsckReport> {
    let superblock =
        read_superblock(device).map_err(|error| with_context("superblock", &error))?;
    let entries =
        load_journal_image(device, superblock).map_err(|error| with_context("journal", &error))?;
    audit_journal(superblock, &entries)
}

fn audit_journal(superblock: Superblock, entries: &[JournalEntry]) -> io::Result<FsckReport> {
    let reserved_blocks = superblock.reserved_blocks();
    let data_blocks = superblock
        .total_blocks
        .checked_sub(reserved_blocks)
        .ok_or_else(|| invalid_data("reserved metadata exceeds filesystem size"))?;

    let mut active = None;
    let mut journal_writes = 0_usize;
    let mut committed_transactions = 0_usize;

    for entry in entries {
        match entry {
            JournalEntry::Begin { txid } => {
                if active.is_some() {
                    return Err(invalid_data("nested journal transaction"));
                }
                active = Some(*txid);
            }
            JournalEntry::Write { txid, block, .. } => {
                if active != Some(*txid) {
                    return Err(invalid_data(
                        "journal write does not match active transaction",
                    ));
                }
                if *block < reserved_blocks || *block >= superblock.total_blocks {
                    return Err(invalid_data(
                        "journal write targets reserved or invalid block",
                    ));
                }
                journal_writes = journal_writes
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("journal write count overflow"))?;
            }
            JournalEntry::Commit { txid } => {
                if active != Some(*txid) {
                    return Err(invalid_data(
                        "journal commit does not match active transaction",
                    ));
                }
                active = None;
                committed_transactions = committed_transactions
                    .checked_add(1)
                    .ok_or_else(|| invalid_data("committed transaction count overflow"))?;
            }
        }
    }

    Ok(FsckReport {
        total_blocks: superblock.total_blocks,
        reserved_blocks,
        data_blocks,
        journal_entries: entries.len(),
        journal_writes,
        committed_transactions,
        pending_transaction: active,
    })
}

fn with_context(layer: &'static str, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("fsck {layer}: {error}"))
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BLOCK_SIZE;

    #[test]
    fn audit_reports_committed_and_pending_transactions() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let data = Box::new([7_u8; BLOCK_SIZE]);
        let entries = vec![
            JournalEntry::Begin { txid: 1 },
            JournalEntry::Write {
                txid: 1,
                block: superblock.reserved_blocks(),
                data: data.clone(),
            },
            JournalEntry::Commit { txid: 1 },
            JournalEntry::Begin { txid: 2 },
            JournalEntry::Write {
                txid: 2,
                block: superblock.reserved_blocks() + 1,
                data,
            },
        ];

        let report = audit_journal(superblock, &entries).unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.pending_transaction, Some(2));
        assert_eq!(report.journal_writes, 2);
        assert_eq!(report.data_blocks, 13);
    }

    #[test]
    fn audit_rejects_reserved_block_ownership() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let entries = vec![
            JournalEntry::Begin { txid: 1 },
            JournalEntry::Write {
                txid: 1,
                block: superblock.journal_start,
                data: Box::new([0_u8; BLOCK_SIZE]),
            },
        ];

        assert_eq!(
            audit_journal(superblock, &entries).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
