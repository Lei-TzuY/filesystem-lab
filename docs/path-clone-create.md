# Pathname clone-to-new-file

`clone_file_blocks_to_path_journaled` snapshots a non-empty contiguous range of complete logical blocks from an existing regular file named by an absolute pathname and creates a fresh destination pathname containing independent physical copies.

`clone_file_to_path_journaled` builds on the same contract to clone the source's entire logical-block vector. It also supports zero-block regular files by creating a fresh empty regular-file destination without allocating data blocks.

## Contract

The operations first recover and checkpoint any older committed WAL, then resolve the source pathname with the normal bounded symbolic-link rules. A whole-file clone requires the source to resolve to a regular file and derives its complete size from the persisted logical-block-vector length. For a non-empty source, it reads the entire selected logical-block range into memory before destination mutation begins. The snapshot is then passed to the existing multi-block pathname create transaction, which resolves the destination parent, allocates one fresh physical block per logical block, creates a fresh regular-file inode and namespace entry, and publishes allocator, inode, directory, and data images under one WAL commit. A zero-block whole-file clone uses the existing atomic empty-file pathname create transaction instead.

The source inode, namespace entries, block references, allocator ownership, and data images are unchanged. For non-empty clones, destination physical blocks must be distinct from all source blocks; this is a physical copy, not a reflink or shared-extent operation.

For the block-range API, a zero block count or out-of-range source interval is rejected. Both APIs reject a non-file source, malformed pathname, destination collision, allocator exhaustion, or insufficient journal capacity before a committed destination can become visible.

## Crash semantics

Deterministic write/flush crash enumeration exercises both the bounded range clone-create and whole-file clone operations. After reboot and recovery, exactly one of two durable outcomes is allowed:

- the destination is absent and allocator/inode/directory state matches the pre-operation state; or
- the destination exists as a complete regular file containing every requested source block image in order, backed by newly allocated uniquely owned physical blocks. For a zero-block whole-file clone, the complete outcome is a fresh zero-block inode with no data allocation.

The source must remain unchanged in both outcomes. Post-recovery allocator validation, unique physical-reference ownership, read-only fsck, empty checkpointed journal, and idempotent second recovery are required.

## Format boundary

Filesystem format remains **v5**. No inode, directory, allocation, journal, or data-block encoding changes are introduced. Because v5 does not persist byte EOF, these APIs operate on complete 4 KiB logical blocks. Whole-file clone means the complete persisted block vector; it does not claim byte-precise EOF. Neither API defines partial-block EOF, sparse holes, reflinks, shared extents, copy-on-write, or extent metadata.
