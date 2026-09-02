use std::collections::BTreeMap;
use std::io;

use crate::block::BLOCK_SIZE;

pub type TransactionId = u64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum JournalEntry {
    Begin {
        txid: TransactionId,
    },
    Write {
        txid: TransactionId,
        block: u64,
        data: Box<[u8; BLOCK_SIZE]>,
    },
    Commit {
        txid: TransactionId,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct JournalImage {
    entries: Vec<JournalEntry>,
}

impl JournalImage {
    #[must_use]
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Replays only transactions with a complete begin/write/commit sequence.
    ///
    /// Writes belonging to a transaction whose commit marker is absent are ignored. Committed
    /// transactions are applied in log order, and later writes to the same block replace earlier
    /// writes.
    ///
    /// # Errors
    ///
    /// Returns an error if the journal image is structurally malformed, including nested
    /// transactions, transaction-id mismatches, writes outside a transaction, or duplicate commits.
    pub fn replay_into(&self, home: &mut BTreeMap<u64, [u8; BLOCK_SIZE]>) -> io::Result<usize> {
        let mut active: Option<TransactionId> = None;
        let mut pending = Vec::<(u64, [u8; BLOCK_SIZE])>::new();
        let mut replayed = 0_usize;

        for entry in &self.entries {
            match entry {
                JournalEntry::Begin { txid } => {
                    if active.is_some() {
                        return Err(invalid_journal("nested journal transaction"));
                    }
                    active = Some(*txid);
                    pending.clear();
                }
                JournalEntry::Write { txid, block, data } => {
                    if active != Some(*txid) {
                        return Err(invalid_journal(
                            "journal write does not match active transaction",
                        ));
                    }
                    pending.push((*block, **data));
                }
                JournalEntry::Commit { txid } => {
                    if active != Some(*txid) {
                        return Err(invalid_journal(
                            "journal commit does not match active transaction",
                        ));
                    }
                    for (block, data) in pending.drain(..) {
                        home.insert(block, data);
                    }
                    active = None;
                    replayed = replayed.checked_add(1).ok_or_else(|| {
                        invalid_journal("committed transaction count overflowed usize")
                    })?;
                }
            }
        }

        Ok(replayed)
    }

    /// Validates journal structure without modifying a home image.
    ///
    /// An incomplete transaction at the end of the log is valid because it models a crash before
    /// the commit marker became durable.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed transaction ordering or transaction-id mismatches.
    pub fn validate(&self) -> io::Result<()> {
        let mut sink = BTreeMap::new();
        self.replay_into(&mut sink).map(|_| ())
    }
}

#[derive(Debug, Default)]
pub struct JournalLog {
    entries: Vec<JournalEntry>,
    active: Option<TransactionId>,
    next_txid: TransactionId,
}

impl JournalLog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    #[must_use]
    pub fn active_transaction(&self) -> Option<TransactionId> {
        self.active
    }

    /// Starts one transaction and appends its begin record.
    ///
    /// # Errors
    ///
    /// Returns an error if another transaction is active or transaction identifiers are exhausted.
    pub fn begin(&mut self) -> io::Result<TransactionId> {
        if self.active.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "a journal transaction is already active",
            ));
        }

        let txid = self.next_txid;
        self.next_txid = self
            .next_txid
            .checked_add(1)
            .ok_or_else(|| io::Error::other("journal transaction id exhausted"))?;
        self.entries.push(JournalEntry::Begin { txid });
        self.active = Some(txid);
        Ok(txid)
    }

    /// Appends one full-block write intent to the active transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if `txid` is not the currently active transaction.
    pub fn write(
        &mut self,
        txid: TransactionId,
        block: u64,
        data: [u8; BLOCK_SIZE],
    ) -> io::Result<()> {
        self.require_active(txid)?;
        self.entries.push(JournalEntry::Write {
            txid,
            block,
            data: Box::new(data),
        });
        Ok(())
    }

    /// Appends the commit marker for the active transaction.
    ///
    /// A commit marker is the logical replay boundary: a crash image containing it replays all
    /// preceding writes in the transaction, while a prefix ending before it replays none of them.
    ///
    /// # Errors
    ///
    /// Returns an error if `txid` is not the currently active transaction.
    pub fn commit(&mut self, txid: TransactionId) -> io::Result<()> {
        self.require_active(txid)?;
        self.entries.push(JournalEntry::Commit { txid });
        self.active = None;
        Ok(())
    }

    /// Produces the durable journal prefix visible after a deterministic crash point.
    ///
    /// # Errors
    ///
    /// Returns an error when `durable_entries` exceeds the current log length.
    pub fn crash_prefix(&self, durable_entries: usize) -> io::Result<JournalImage> {
        if durable_entries > self.entries.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "crash prefix exceeds journal length",
            ));
        }
        Ok(JournalImage {
            entries: self.entries[..durable_entries].to_vec(),
        })
    }

    #[must_use]
    pub fn durable_image(&self) -> JournalImage {
        JournalImage {
            entries: self.entries.clone(),
        }
    }

    fn require_active(&self, txid: TransactionId) -> io::Result<()> {
        if self.active != Some(txid) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "transaction id is not active",
            ));
        }
        Ok(())
    }
}

fn invalid_journal(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_before_commit_discards_transaction() {
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        log.write(txid, 7, [0xA5; BLOCK_SIZE]).unwrap();

        let image = log.crash_prefix(log.entries().len()).unwrap();
        let mut home = BTreeMap::new();
        assert_eq!(image.replay_into(&mut home).unwrap(), 0);
        assert!(home.is_empty());
    }

    #[test]
    fn commit_marker_is_atomic_replay_boundary() {
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        let first = [1_u8; BLOCK_SIZE];
        let second = [2_u8; BLOCK_SIZE];
        log.write(txid, 3, first).unwrap();
        log.write(txid, 4, second).unwrap();
        let before_commit = log.entries().len();
        log.commit(txid).unwrap();

        let mut home = BTreeMap::new();
        assert_eq!(
            log.crash_prefix(before_commit)
                .unwrap()
                .replay_into(&mut home)
                .unwrap(),
            0
        );
        assert!(home.is_empty());

        assert_eq!(log.durable_image().replay_into(&mut home).unwrap(), 1);
        assert_eq!(home.get(&3), Some(&first));
        assert_eq!(home.get(&4), Some(&second));
    }

    #[test]
    fn committed_transactions_replay_in_log_order() {
        let mut log = JournalLog::new();
        let first_tx = log.begin().unwrap();
        log.write(first_tx, 5, [1; BLOCK_SIZE]).unwrap();
        log.commit(first_tx).unwrap();

        let second_tx = log.begin().unwrap();
        let latest = [9; BLOCK_SIZE];
        log.write(second_tx, 5, latest).unwrap();
        log.commit(second_tx).unwrap();

        let mut home = BTreeMap::new();
        assert_eq!(log.durable_image().replay_into(&mut home).unwrap(), 2);
        assert_eq!(home.get(&5), Some(&latest));
    }

    #[test]
    fn active_transaction_is_exclusive_and_ids_are_monotonic() {
        let mut log = JournalLog::new();
        let first = log.begin().unwrap();
        assert_eq!(log.begin().unwrap_err().kind(), io::ErrorKind::WouldBlock);
        log.commit(first).unwrap();
        let second = log.begin().unwrap();
        assert!(second > first);
    }

    #[test]
    fn mismatched_transaction_operations_are_rejected() {
        let mut log = JournalLog::new();
        let txid = log.begin().unwrap();
        assert_eq!(
            log.write(txid + 1, 1, [0; BLOCK_SIZE]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            log.commit(txid + 1).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn malformed_images_are_rejected() {
        let image = JournalImage {
            entries: vec![JournalEntry::Commit { txid: 42 }],
        };
        let mut home = BTreeMap::new();
        assert_eq!(
            image.replay_into(&mut home).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn crash_prefix_bounds_are_checked() {
        let log = JournalLog::new();
        assert_eq!(
            log.crash_prefix(1).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
