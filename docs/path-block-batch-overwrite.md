# Pathname non-contiguous block batch overwrite

`write_file_blocks_at_path_journaled` exposes the existing format-v5 `write_file_blocks_journaled` primitive through bounded absolute-path resolution. Intermediate and final symbolic links are followed using the repository-wide resolver, then the resolved inode ID is delegated to the existing durable batch-overwrite implementation.

The operation atomically replaces any non-empty set of distinct, already-existing logical blocks in one regular file. The logical indices do not need to be contiguous. It does not allocate or free blocks, change inode references, alter namespace metadata, extend the file, create holes, or define byte-length/EOF/extent semantics. Filesystem format remains v5.

Durability remains entirely in the inode-level primitive: all changed data-block images are emitted in one WAL transaction, committed transactions are replayed to home locations, and the fixed journal reservation is checkpointed before successful return. Invalid batches are rejected before WAL publication.

The pathname crash matrix enumerates every injected write/flush interruption and requires recovery to expose either the complete old batch or the complete new batch, never a mixed result. It also verifies allocator, inode table, and namespace stability, fsck cleanliness, an empty checkpointed journal, and idempotent second recovery.
