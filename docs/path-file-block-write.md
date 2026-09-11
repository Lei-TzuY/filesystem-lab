# Pathname whole-block regular-file write

`write_file_blocks_at_path_journaled()` provides a bounded format-v5 mutation surface for replacing every already-persisted complete logical block of one regular file selected by absolute pathname.

The operation first recovers and checkpoints any older committed WAL, then resolves the pathname with bounded intermediate and final symbolic-link following. The resolved inode must be a regular file. The supplied byte slice must be exactly `logical_blocks * 4096` bytes, where `logical_blocks` is the number of blocks already referenced by that inode.

For a non-empty file the implementation delegates to the existing journaled range-write primitive with block index and byte offset zero. That keeps allocator ownership checks, journal-capacity validation, WAL publication, recovery ordering, and crash-consistent data replacement in the existing mutation path rather than introducing a second durability mechanism.

A zero-block file accepts only an empty slice. After recovery/checkpoint this is a no-op and publishes no new file-data transaction. Any other length mismatch is rejected before the range-write transaction is invoked.

## Whole-file block replacement with resizing

`replace_file_at_path_journaled()` replaces the complete persisted logical-block sequence while allowing the block count to change, including transitions to or from an empty file. Path resolution has the same recovery-before-lookup and final-symlink-follow behavior as the fixed-size whole-file write.

The wrapper chooses exactly one existing crash-consistent transaction from recovered inode state:

- empty to empty: no-op after recovery/checkpoint;
- empty to non-empty: atomic multi-block append;
- non-empty to empty: truncate-to-zero;
- non-empty to non-empty: variable-length range replacement spanning the complete current block list.

The operation never implements a resize as multiple independently durable mutations. Each state-changing branch is a single existing WAL transaction, so allocator ownership, inode block-list changes, data publication, recovery, checkpointing, and journal-capacity validation remain centralized in their established primitives.

This capability remains block-granular. Format v5 still has no persisted byte EOF, so neither whole-file API claims partial-final-block, sparse-hole, extent, or byte-length semantics.

No superblock, allocation image, inode record, directory entry, journal record, or data-block encoding changes. The filesystem remains **format v5** and requires no migration.
