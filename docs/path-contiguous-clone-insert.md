# Contiguous pathname clone insertion

`clone_file_blocks_contiguous_insert_at_path_journaled()` copies a non-empty range of complete logical blocks from a resolved regular-file source and inserts independent copies at an existing logical-block boundary in a resolved regular-file destination.

Before either pathname is resolved, any committed journal state is recovered and checkpointed. The source bytes are snapshotted before destination mutation, so source and destination may resolve to the same inode without insertion shifting the data selected for cloning. Fresh destination blocks are allocated as the lowest-address first-fit contiguous run large enough for the complete source range. Allocation ownership, destination inode growth, and copied data images are published through one existing WAL transaction.

The operation is deliberately block-granular. Filesystem format v5 continues to persist explicit inode block vectors; the contiguous run is an allocation-time property only. This does not add a persistent extent encoding, shared-block reflink/COW semantics, sparse holes, byte-level EOF, or general POSIX copy semantics.

Deterministic crash-prefix tests require recovery to expose either the complete old destination or the complete inserted clone. They also verify source immutability for cross-file cloning, unique physical ownership, allocation accounting, unchanged namespace state, clean `fsck`, journal clearing, and idempotent second recovery.
