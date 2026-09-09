# Journal replacement safety

Filesystem format v5 uses one bounded persistent journal reservation. A non-empty journal image can be the only durable recovery source for a transaction whose commit reached storage before all home-location writes became durable.

`store_journal_image()` therefore refuses to publish a new non-empty image while any older non-empty image is still present. The operation returns `WouldBlock` without modifying the journal reservation. Callers must run recovery and checkpoint the existing image, then recompute and retry the filesystem mutation from the recovered home state.

This ordering matters because recovering only at the moment of journal replacement would be too late: a higher-level mutation may already have derived allocator, inode, directory, or data updates from stale home blocks. Refusing publication preserves the prior WAL and forces the retry to start from a recovered state.

The rule does not change the v5 on-disk encoding. Empty-image writes remain available for explicit journal initialization/checkpoint behavior, and malformed existing journal images are rejected by normal journal decoding before a replacement can proceed.

## Durability invariant

For any attempted second transaction while an older journal image remains durable, exactly one of these states is permitted:

1. the old journal remains authoritative and the second transaction is not published; or
2. the caller explicitly recovers and checkpoints the old transaction, recomputes the second mutation, and only then publishes its new WAL.

A later transaction must never destroy the only durable copy of an earlier committed transaction.
