# Pathname regular-file zero-range

`zero_file_range_at_path_journaled` adds a pathname-facing mutation surface over the existing crash-consistent regular-file zero-range primitive.

The absolute path is resolved with the existing bounded symbolic-link rules, including intermediate and final symlink following. The resolved inode is then passed to `zero_file_range_journaled`, which requires a non-empty byte range wholly contained in already allocated logical blocks and publishes the changed data blocks through the existing WAL/recovery/checkpoint path.

This slice does not change filesystem format v5. It does not allocate or free blocks, modify inode block references or namespace metadata, extend files, infer EOF, create sparse holes, or define POSIX `fallocate` semantics.

Deterministic write/flush crash enumeration requires recovery to expose either the complete pre-operation bytes or the complete zeroed range. Allocator state, inode references, and namespace entries must remain byte-for-byte unchanged; fsck must succeed, the journal must be checkpointed empty, and a second recovery must be idempotent.
