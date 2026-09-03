use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::format::Superblock;
use crate::journal::{JournalEntry, JournalImage};
use crate::journal_region::load_journal_image;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsckReport {
    pub total_blocks: u64,
    pub reserved_blocks: u64,
    pub data_blocks: u64,
    pub allocated_blocks: u64,
    pub free_blocks: u64,
    pub journal_writes: usize,
    pub committed_transactions: usize,
    pub pending_transaction: Option<u64>,
}

/// Audits the durable filesystem layers that currently have defined on-disk formats.
///
/// This checker is deliberately read-only. It validates the superblock/device geometry, the
/// persistent journal image and transaction state, and the durable allocation bitmap accounting.
/// A trailing uncommitted transaction is a valid crash state and is reported rather than treated
/// as corruption.
///
/// # Errors
///
/// Returns an error when any durable metadata layer is malformed or when a journal write targets
/// a forbidden home location.
pub fn check_device<D: BlockDevice>(device: &mut D) -> io::Result<FsckReport> {
    let superblock = Superblock::read_from(device)?;
    let image = load_journal_image(device, superblock)?;
    let allocator = load_allocator(device, superblock)?;
    audit_journal(
        superblock,
        image.entries(),
        allocator.allocated_blocks(),
        allocator.free_blocks(),
    )
}

fn audit_journal(
    superblock: Superblock,
    entries: &[JournalEntry],
    allocated_blocks: u64,
    free_blocks: u64,
) -> io::Result<FsckReport> {
    let data_blocks = superblock
        .total_blocks
        .checked_sub(superblock.reserved_blocks())
        .ok_or_else(|| invalid_data("reserved metadata exceeds filesystem size"))?;
    if allocated_blocks
        .checked_add(free_blocks)
        .filter(|total| *total == data_blocks)
        .is_none()
    {
        return Err(invalid_data("allocation accounting does not cover data blocks"));
    }

    let mut active = None;
    let mut journal_writes = 0_usize;
    let mut committed_transactions = 0_usize;

    for entry in entries {
        match entry {
            JournalEntry::Begin { txid } => {
                if active.replace(*txid).is_some() {
                    return Err(invalid_data("nested journal transaction"));
                }
            }
            JournalEntry::Write { txid, block, .. } => {
                if active != Some(*txid) {
                    return Err(invalid_data("journal write outside active transaction"));
                }
                validate_home_block(superblock, *block)?;
                journal_writes += 1;
            }
            JournalEntry::Commit { txid } => {
                if active != Some(*txid) {
                    return Err(invalid_data("journal commit outside active transaction"));
                }
                active = None;
                committed_transactions += 1;
            }
        }
    }

    Ok(FsckReport {
        total_blocks: superblock.total_blocks,
        reserved_blocks: superblock.reserved_blocks(),
        data_blocks,
        allocated_blocks,
        free_blocks,
        journal_writes,
        committed_transactions,
        pending_transaction: active,
    })
}

fn validate_home_block(superblock: Superblock, block: u64) -> io::Result<()> {
    if block >= superblock.total_blocks {
        return Err(invalid_data("journal write target is outside filesystem"));
    }
    if block == 0 || superblock.journal_range().contains(&block) {
        return Err(invalid_data("journal write targets protected metadata"));
    }
    Ok(())
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
        let data_blocks = superblock.total_blocks - superblock.reserved_blocks();

        let report = audit_journal(superblock, &entries, 0, data_blocks).unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.pending_transaction, Some(2));
        assert_eq!(report.journal_writes, 2);
        assert_eq!(report.data_blocks, data_blocks);
        assert_eq!(report.allocated_blocks, 0);
        assert_eq!(report.free_blocks, data_blocks);
    }

    #[test]
    fn audit_accepts_allocation_metadata_as_journal_home() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let entries = vec![
            JournalEntry::Begin { txid: 1 },
            JournalEntry::Write {
                txid: 1,
                block: superblock.allocation_start,
                data: Box::new([0_u8; BLOCK_SIZE]),
            },
            JournalEntry::Commit { txid: 1 },
        ];
        let data_blocks = superblock.total_blocks - superblock.reserved_blocks();

        let report = audit_journal(superblock, &entries, 0, data_blocks).unwrap();
        assert_eq!(report.committed_transactions, 1);
    }

    #[test]
    fn audit_rejects_superblock_and_journal_ownership() {
        let superblock = Superblock::with_journal_blocks(16, 2).unwrap();
        let data_blocks = superblock.total_blocks - superblock.reserved_blocks();

        for block in [0, superblock.journal_start] {
            let entries = vec![
                JournalEntry::Begin { txid: 1 },
                JournalEntry::Write {
                    txid: 1,
                    block,
                    data: Box::new([0_u8; BLOCK_SIZE]),
                },
                JournalEntry::Commit { txid: 1 },
            ];
            let err = audit_journal(superblock, &entries, 0, data_blocks).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
    }
}
