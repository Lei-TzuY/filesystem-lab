# Regular-file block-range clone replacement

`clone_file_blocks_replace_journaled` snapshots a non-empty contiguous source logical-block range and atomically replaces an equal-length existing destination logical-block range with freshly allocated physical copies.

The source and destination must be distinct regular-file inodes. Source images are read before any metadata mutation. Replacement blocks are allocated while every displaced destination block is still allocator-owned, so the replacement homes cannot alias the blocks they displace. The displaced blocks are then released and one WAL transaction publishes:

- allocation metadata owning every replacement block and freeing every displaced block;
- the inode-table image containing the replacement destination references; and
- every replacement data-block image containing the snapshotted source bytes.

The source mapping and data remain unchanged. Destination block count and the relative ordering of all unaffected destination blocks are preserved. The operation does not provide shared-block or reflink semantics.

`clone_file_blocks_replace_at_path_journaled` exposes the same primitive through absolute pathname resolution. Before either pathname is resolved, any older committed WAL is recovered and checkpointed so source and destination inode selection cannot observe a partially replayed namespace. Both paths then follow intermediate and final symbolic links under the existing bounded expansion rules before the resolved inode IDs are passed to the inode-level operation. The pathname layer does not introduce a second durability mechanism or any new on-disk state.

`clone_file_to_existing_path_journaled` extends the pathname surface to complete persisted files and permits the destination block count to change. It first recovers/checkpoints and resolves both endpoints, rejects aliases that resolve to the same inode, snapshots every complete logical block from the source, then delegates destination publication to `replace_file_at_path_journaled`. The source is never mutated. The destination may grow, shrink, become empty, or grow from empty, but each state-changing destination branch remains exactly one existing WAL transaction. Destination blocks are freshly allocated physical copies rather than shared references.

## Crash contract

Deterministic crash enumeration covers WAL publication, allocation/inode/data home replay, journal clearing, and checkpoint durability boundaries. Before a durable commit, recovery preserves the old destination references and ownership. After a durable commit, recovery converges to the complete replacement state.

After successful recovery:

- every replacement destination reference names a newly allocator-owned block;
- every displaced destination block is free;
- source mappings and source data are unchanged;
- cloned data images equal the source snapshot;
- allocator owned/free accounting has no double ownership;
- namespace invariants remain unchanged and fsck is clean;
- the journal is empty; and
- a second recovery/checkpoint is a no-op.

For equal-length range replacement, destination block count and unaffected ordering remain unchanged. For whole-file clone replacement, the destination block vector may change length, but crash recovery must expose either the complete pre-operation destination or the complete source snapshot, never a mixed-length or mixed-data image.

The pathname crash matrices additionally verify that symlink traversal never changes namespace state and that every interrupted operation recovers either the complete pre-operation allocator/inode image or the complete post-operation image, never a mixed state. A dedicated recovery-boundary matrix also interrupts creation of a final source symlink after durable commit but before complete home replay, then calls pathname clone-replace without an external recovery step. The pathname entry point must recover and checkpoint that older WAL before resolving either endpoint, after which replacement data, allocator accounting, released-block ownership, inode references, fsck, journal clearing, and second-recovery idempotence are all revalidated.

## Format scope

This remains a format-v5 block-granular primitive. A whole file is exactly its persisted vector of complete 4 KiB logical blocks. No operation here defines persisted byte length, partial-final-block EOF, sparse holes, extents, reflinks, or broad POSIX behavior, and no on-disk migration is required.
