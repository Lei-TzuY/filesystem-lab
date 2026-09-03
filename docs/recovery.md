# Recovery semantics

The durable recovery implementation consumes the bounded journal-region image and replays committed transactions to their home blocks.

## Ordering contract

1. The complete journal-region image is loaded and validated before any home-location write is issued.
2. Writes remain pending until their matching `Commit` record is encountered.
3. A trailing transaction without a durable commit record is ignored completely.
4. Committed home writes are issued in journal order.
5. After all committed writes have been issued, one block-device `flush` establishes the home-location durability boundary.

The journal is not cleared by recovery in this milestone. Replaying the same durable journal is therefore intentionally idempotent: a crash or I/O failure after any prefix of home writes can be followed by another recovery pass, which overwrites already-applied blocks with the same contents and completes the remaining writes.

## Allowed home locations

Journal writes may target ordinary data blocks and the allocation-metadata home region. The superblock and the journal reservation itself remain forbidden targets so recovery can never overwrite the metadata that defines filesystem geometry or the log that is currently driving replay.

`allocation_tx::store_allocator_journaled` renders the complete checksummed allocation image, records every allocation home block in one transaction, persists the committed journal image, and only then invokes normal recovery to write the allocation image home. A failure during the home-write phase leaves the committed journal available for idempotent retry.

The allocator transaction is deliberately whole-image and bounded. If the journal reservation is too small to contain every allocation-image block plus transaction framing, the update is rejected rather than split across commits. This keeps the atomicity argument explicit while the laboratory still uses a bounded non-circular journal.

## Safety properties

The journal-region loader validates checksums, record framing, transaction ordering, device geometry, and target-block bounds before recovery mutates home blocks. Crash-before-commit therefore leaves allocation metadata unchanged; crash or I/O failure after a durable commit can be repaired by replaying the same journal until the home flush succeeds.

This milestone does **not** define checkpointing, journal clearing, circular head/tail state, generation numbers, persistent inode metadata, or namespace persistence. Those require later bounded milestones and, where the durable schema changes, explicit format versioning.
