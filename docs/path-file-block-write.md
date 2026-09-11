# Pathname whole-block regular-file write

`write_file_blocks_at_path_journaled()` provides a bounded format-v5 mutation surface for replacing every already-persisted complete logical block of one regular file selected by absolute pathname.

The operation first recovers and checkpoints any older committed WAL, then resolves the pathname with bounded intermediate and final symbolic-link following. The resolved inode must be a regular file. The supplied byte slice must be exactly `logical_blocks * 4096` bytes, where `logical_blocks` is the number of blocks already referenced by that inode.

For a non-empty file the implementation delegates to the existing journaled range-write primitive with block index and byte offset zero. That keeps allocator ownership checks, journal-capacity validation, WAL publication, recovery ordering, and crash-consistent data replacement in the existing mutation path rather than introducing a second durability mechanism.

A zero-block file accepts only an empty slice. After recovery/checkpoint this is a no-op and publishes no new file-data transaction. Any other length mismatch is rejected before the range-write transaction is invoked.

This API deliberately does not define byte-level EOF. It cannot grow or shrink a file, allocate blocks, create sparse holes, or encode a partial final logical block. Those semantics are not representable in the current inode schema.

No superblock, allocation image, inode record, directory entry, journal record, or data-block encoding changes. The filesystem remains **format v5** and requires no migration.
