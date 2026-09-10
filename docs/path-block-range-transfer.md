# Pathname logical-block range transfer

`transfer_file_block_range_at_path_journaled` composes bounded absolute pathname resolution with the existing format-v5 `transfer_file_block_range_journaled` transaction.

Before resolving either endpoint, the pathname API recovers and checkpoints any older committed WAL. This prevents source or destination inode selection from observing a partially replayed namespace. Both source and destination then follow intermediate and final symbolic links under the repository-wide bounded expansion rules. After resolution, one non-empty contiguous source block-reference range is removed from the source regular-file inode and inserted at a destination logical boundary. Physical data blocks are not copied or rewritten and allocator ownership is unchanged. The inode-table change is published through one WAL transaction, so recovery cannot leave a transferred block referenced by both files or by neither file.

The operation rejects identical resolved inodes, non-file endpoints, empty or overflowing source ranges, source ranges beyond file end, destination boundaries beyond the destination block vector, and allocator/reference disagreement before WAL publication. Recovery/checkpoint and pathname-resolution failures are propagated before a new transfer transaction is started.

Format v5 has no persisted byte length. This API is block-granular and intentionally does not claim byte-range move, EOF, sparse-hole, extent, reflink, or POSIX semantics.

Deterministic crash coverage enumerates every write/flush interruption of the transfer itself and requires recovery to produce either the complete old inode table or the complete transferred inode table. A separate recovery-boundary matrix enumerates crashes while publishing a source symlink, selects reboot states whose durable WAL already contains Commit, and invokes pathname transfer without an external recovery call. Those states must first recover the namespace, then transfer the requested block with unchanged physical ownership, exact allocator/inode accounting, unique surviving physical references, fsck cleanliness, an empty checkpointed journal, and idempotent second recovery.
