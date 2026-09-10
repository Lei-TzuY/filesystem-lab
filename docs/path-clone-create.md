# Pathname clone-to-new-file

`clone_file_blocks_to_path_journaled` snapshots a non-empty contiguous range of complete logical blocks from an existing regular file named by an absolute pathname and creates a fresh destination pathname containing independent physical copies.

## Contract

The operation first recovers and checkpoints any older committed WAL, then resolves the source pathname with the normal bounded symbolic-link rules. It reads the entire requested logical-block range into memory before destination mutation begins. The snapshot is then passed to the existing multi-block pathname create transaction, which resolves the destination parent, allocates one fresh physical block per logical block, creates a fresh regular-file inode and namespace entry, and publishes allocator, inode, directory, and data images under one WAL commit.

The source inode, namespace entries, block references, allocator ownership, and data images are unchanged. Destination physical blocks must be distinct from all source blocks; this is a physical copy, not a reflink or shared-extent operation.

A zero block count, out-of-range source interval, non-file source, malformed pathname, destination collision, allocator exhaustion, or insufficient journal capacity is rejected before a committed destination can become visible.

## Crash semantics

Deterministic write/flush crash enumeration exercises the complete pathname clone-create operation. After reboot and recovery, exactly one of two durable outcomes is allowed:

- the destination is absent and allocator/inode/directory state matches the pre-operation state; or
- the destination exists as a complete regular file containing every requested source block image in order, backed by newly allocated uniquely owned physical blocks.

The source must remain unchanged in both outcomes. Post-recovery allocator validation, unique physical-reference ownership, read-only fsck, empty checkpointed journal, and idempotent second recovery are required.

## Format boundary

Filesystem format remains **v5**. No inode, directory, allocation, journal, or data-block encoding changes are introduced. Because v5 does not persist byte EOF, this API clones only complete 4 KiB logical blocks. It does not define partial-block EOF, sparse holes, reflinks, shared extents, copy-on-write, or extent metadata.
