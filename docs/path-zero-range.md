# Pathname regular-file zero-range

`zero_file_range_at_path_journaled` adds a pathname-facing mutation surface over the existing crash-consistent regular-file zero-range primitive.

Before pathname resolution, the wrapper recovers and checkpoints any older durable WAL. This guarantees that inode selection never observes a partially replayed namespace, including when the final pathname component is a symbolic link created by the previous committed transaction. The absolute path is then resolved with the existing bounded symbolic-link rules, including intermediate and final symlink following. The resolved inode is passed to `zero_file_range_journaled`, which requires a non-empty byte range wholly contained in already allocated logical blocks and publishes the changed data blocks through the existing WAL/recovery/checkpoint path.

This slice does not change filesystem format v5. It does not allocate or free blocks as part of zeroing, modify existing file block references or namespace metadata, extend files, infer EOF, create sparse holes, or define POSIX `fallocate` semantics. Recovery of an older committed symlink transaction may of course make that symlink's already-committed inode and owned data block visible before the zero-range transaction begins.

Deterministic write/flush crash enumeration for zero-range itself requires recovery to expose either the complete pre-operation bytes or the complete zeroed range. A separate recovery-boundary matrix enumerates committed crash states while creating a final symlink and then invokes pathname zero-range without external recovery; it verifies recovered namespace visibility, exact allocator/inode accounting, unique physical block ownership, fsck cleanliness, an empty journal after checkpoint, and idempotent second recovery.
