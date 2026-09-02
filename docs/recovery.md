# Recovery semantics

The first durable recovery implementation consumes the bounded journal-region image and replays committed transactions to their home blocks.

## Ordering contract

1. The complete journal-region image is loaded and validated before any home-location write is issued.
2. Writes remain pending until their matching `Commit` record is encountered.
3. A trailing transaction without a durable commit record is ignored completely.
4. Committed home writes are issued in journal order.
5. After all committed writes have been issued, one block-device `flush` establishes the home-location durability boundary.

The journal is not cleared by recovery in this milestone. Replaying the same durable journal is therefore intentionally idempotent: a crash or I/O failure after any prefix of home writes can be followed by another recovery pass, which overwrites already-applied blocks with the same contents and completes the remaining writes.

## Safety properties

The journal-region loader validates checksums, record framing, transaction ordering, device geometry, and target-block bounds before recovery mutates home blocks. Journal writes may not target the superblock or journal reservation itself.

This milestone does **not** define checkpointing, journal clearing, circular head/tail state, generation numbers, allocator persistence, inode persistence, or namespace persistence. Those require later bounded milestones and, where the durable schema changes, explicit format versioning.
