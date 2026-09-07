# Regular-file block-range clone append

`clone_file_blocks_append_journaled` copies a non-empty contiguous range of complete logical blocks from an existing regular file and appends fresh physical copies to an existing destination regular file.

The source images are read before destination metadata is mutated. Fresh physical blocks are then allocated and the following homes are published by one WAL transaction:

- allocation metadata owning every newly allocated block;
- the destination inode table image containing the appended block references;
- every newly allocated data-block image containing the snapshotted source bytes.

The source inode mapping and source data remain unchanged. Source and destination may be the same inode; because source images are snapshotted before allocation and publication, same-inode clone append has snapshot semantics rather than feeding appended copies back into the source range.

## Pathname surface

`clone_file_blocks_append_at_path_journaled` exposes the same transaction through bounded absolute-path resolution. Source and destination both follow intermediate and final symbolic links, then the resolved inode IDs are delegated to the inode-ID primitive without changing allocation, WAL, recovery, or data-publication semantics.

The pathname wrapper deliberately does not introduce independent durability logic. Missing/dangling paths, non-file endpoints, empty or out-of-range source intervals, allocator exhaustion, ownership disagreement, and journal-capacity failures are rejected before a successful publication can be reported.

## Crash contract

Before the commit is durable, recovery must leave the destination mapping and allocator in the old state. After the commit is durable, recovery replays all metadata and cloned data homes so the destination contains the complete appended range. A crash may expose a prefix of home writes before recovery, but mixed allocation/inode metadata must not be accepted by fsck as a valid completed filesystem state.

Deterministic crash enumeration verifies:

- no double ownership and exact owned/free accounting for newly allocated blocks;
- source inode references and source data remain unchanged;
- destination references are either entirely old or, after recovery, entirely appended;
- cloned data homes equal the snapshotted source images after recovery;
- namespace references remain unchanged and fsck is clean after recovery;
- journal checkpointing clears the committed transaction;
- a second recovery/checkpoint is idempotent.

## Format scope

This operation does not change filesystem format v5. The format has no persisted byte length, so the API is intentionally block-granular and does not define EOF growth, partial-block copies, sparse holes, extents, reflinks, or broader POSIX copy semantics.
