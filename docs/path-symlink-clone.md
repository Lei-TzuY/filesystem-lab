# Pathname symbolic-link clone

`clone_symlink_at_path_journaled()` copies the persisted target string of one symbolic link into a fresh destination pathname without following the source's final symlink component.

## Ordering and durability

1. The source is read through `read_symlink_at_path()`, which first recovers and checkpoints older committed WAL and then resolves intermediate symlinks while preserving the final link inode.
2. The validated target string is passed to `create_symlink_at_path_journaled()`.
3. Destination parent resolution again runs after recovery/checkpoint and publication uses the existing symlink create WAL transaction spanning allocator, inode table, directory table, and all target payload blocks.

A crash therefore cannot leave a partial destination link. Recovery exposes either no destination entry or a complete independently allocated symbolic link whose target string matches the source.

## Ownership semantics

The operation is a semantic clone, not reflink/COW. The destination receives a fresh inode and fresh physical payload blocks. Source blocks are neither shared nor modified. Both one-block `SYM1` and bounded multi-block `SYM2` source payloads are accepted through the normal validated read path; the destination is encoded canonically for the recovered target string.

Deterministic crash tests enumerate every write/flush interruption point and verify allocator accounting, unique physical ownership, source preservation, namespace consistency, clean `fsck`, empty checkpointed journal, and idempotent second recovery.

## Format compatibility

Filesystem format remains v5. No inode, allocator, directory, journal, `SYM1`, or `SYM2` on-disk encoding changes are introduced, so no migration is required.
