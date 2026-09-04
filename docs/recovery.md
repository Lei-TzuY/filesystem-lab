# Recovery semantics

The durable recovery implementation consumes the bounded format-v5 journal-region image and replays committed transactions to their home blocks.

## Ordering contract

1. The complete journal-region image is loaded and validated before any home-location write is issued.
2. Writes remain pending until their matching `Commit` record is encountered.
3. A trailing transaction without a durable commit record is ignored completely.
4. Committed home writes are issued in journal order.
5. After all committed writes have been issued, one block-device `flush` establishes the home-location durability boundary.
6. `recover_journal_and_checkpoint` may then clear the fixed journal reservation and flush the cleared image. Journal clearing never precedes durable home replay.

`recover_journal` remains intentionally idempotent and leaves the durable log in place. `recover_journal_and_checkpoint` composes that replay with the crash-safe checkpoint step: a crash before the checkpoint flush leaves the prior committed journal durable and replayable; a crash after it exposes an empty journal. This model relies on the repository's existing block-device flush contract and does not claim sector-tear or controller-reordering behavior.

## Allowed home locations

Journal writes may target ordinary data blocks plus the allocation, inode-table, and directory-table home regions. The superblock and journal reservation itself remain forbidden targets so recovery cannot overwrite the geometry that defines the filesystem or the log driving replay.

Metadata lifecycle transactions remain bounded by the fixed journal reservation. Create, unlink, and bounded rename publish complete cross-table or namespace snapshots through one committed WAL transaction, and truncate-to-zero atomically advances allocator ownership together with the inode block list. If a complete logical update does not fit, it is rejected instead of being split across commits.

The block-granular regular-file overwrite path also uses the WAL for its existing allocated data block. A successful `write_file_block_journaled` now performs replay followed by checkpoint before returning success, leaving the fixed journal reservation empty and immediately reusable. A failure after durable commit may still require a subsequent `recover_journal_and_checkpoint` pass; deterministic crash enumeration covers journal publication, home replay, and checkpoint boundaries.

## Safety properties

The journal-region loader validates checksums, record framing, transaction ordering, device geometry, and target-block bounds before recovery mutates home blocks. Crash-before-commit therefore preserves the old durable state. Crash or I/O failure after durable commit can be repaired by replaying the same journal until the home flush succeeds, after which checkpointing can safely discard the log image.

Recovery is designed to converge idempotently: repeated replay of a retained committed journal writes identical full-block images, and once checkpoint has durably cleared the journal, another recover-and-checkpoint pass is a no-op.

Read-only fsck remains independent of recovery and checks the post-recovery allocation/inode/namespace invariants. Deterministic crash tests exercise create, unlink, rename, truncate-to-zero, file-block overwrite, and journal checkpoint boundaries.

## Current limits

Format v5 still uses a bounded fixed journal reservation rather than a circular head/tail log. It does not model sector tearing, controller reordering, or partial-block persistence. File-data I/O is block-granular over already allocated inode block references; byte length, extension, partial-block writes, sparse files, and append semantics remain separate lifecycle contracts.
