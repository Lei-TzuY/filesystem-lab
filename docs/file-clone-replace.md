# Regular-file block-range clone replacement

`clone_file_blocks_replace_journaled` snapshots a non-empty contiguous source logical-block range and atomically replaces an equal-length existing destination logical-block range with freshly allocated physical copies.

The source and destination must be distinct regular-file inodes. Source images are read before any metadata mutation. Replacement blocks are allocated while every displaced destination block is still allocator-owned, so the replacement homes cannot alias the blocks they displace. The displaced blocks are then released and one WAL transaction publishes:

- allocation metadata owning every replacement block and freeing every displaced block;
- the inode-table image containing the replacement destination references; and
- every replacement data-block image containing the snapshotted source bytes.

The source mapping and data remain unchanged. Destination block count and the relative ordering of all unaffected destination blocks are preserved. The operation does not provide shared-block or reflink semantics.

## Crash contract

Deterministic crash enumeration covers WAL publication, allocation/inode/data home replay, journal clearing, and checkpoint durability boundaries. Before a durable commit, recovery preserves the old destination references and ownership. After a durable commit, recovery converges to the complete replacement state.

After successful recovery:

- every replacement destination reference names a newly allocator-owned block;
- every displaced destination block is free;
- source mappings and source data are unchanged;
- destination block count and unaffected ordering are unchanged;
- cloned data images equal the source snapshot;
- allocator owned/free accounting has no double ownership;
- namespace invariants remain unchanged and fsck is clean;
- the journal is empty; and
- a second recovery/checkpoint is a no-op.

## Format scope

This remains a format-v5 block-granular primitive. It does not define persisted byte length, EOF, partial-block replacement, sparse holes, extents, reflinks, same-inode overlap semantics, or broad POSIX behavior.
