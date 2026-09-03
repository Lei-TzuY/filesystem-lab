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

Journal writes may target ordinary data blocks plus the allocation-metadata and inode-table home regions. The superblock and the journal reservation itself remain forbidden targets so recovery can never overwrite the metadata that defines filesystem geometry or the log that is currently driving replay.

`allocation_tx::store_allocator_journaled` renders the complete checksummed allocation image, records every allocation home block in one transaction, persists the committed journal image, and only then invokes normal recovery to write the allocation image home. A failure during the home-write phase leaves the committed journal available for idempotent retry.

`inode_tx::store_inode_table_journaled` renders the complete checksummed inode-table image and compares it with the current durable inode region. Every changed inode-table home block is placed in one transaction; unchanged blocks are omitted. The committed journal is persisted before recovery writes changed inode blocks home. An already-identical inode snapshot is a no-op that does not rewrite the journal.

Both metadata transaction paths are deliberately bounded. If the journal reservation is too small to contain all blocks required for one logical metadata update plus transaction framing, the update is rejected rather than split across commits. This keeps the atomicity argument explicit while the laboratory still uses a bounded non-circular journal.

## Safety properties

The journal-region loader validates checksums, record framing, transaction ordering, device geometry, and target-block bounds before recovery mutates home blocks. Crash-before-commit therefore leaves allocation or inode metadata unchanged; crash or I/O failure after a durable commit can be repaired by replaying the same journal until the home flush succeeds.

Journaled inode-table regressions additionally verify that a committed inode update survives an injected first-home-write failure, that replay is idempotent, and that an update requiring more changed inode blocks than the journal can hold is rejected instead of partially committed.

This milestone does **not** define checkpointing, journal clearing, circular head/tail state, generation numbers, directory persistence, or atomic multi-object allocation+inode transactions. Those require later bounded milestones and, where the durable schema changes, explicit format versioning.
