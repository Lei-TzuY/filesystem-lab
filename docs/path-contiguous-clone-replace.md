# Contiguous pathname clone replacement

`clone_file_blocks_contiguous_replace_at_path_journaled()` copies a non-empty range of complete logical blocks from one regular-file pathname into an equal-length existing destination range. The copied blocks receive fresh physical homes allocated as one lowest-address first-fit contiguous run.

The operation first recovers and checkpoints any older committed WAL, resolves both pathnames with the existing bounded symbolic-link rules, rejects identical resolved inodes, and snapshots the requested source range. Destination publication reuses the contiguous replacement transaction: all fresh blocks are reserved before displaced destination ownership is released, then allocator metadata, destination inode references, and copied data images are committed together through one WAL transaction.

Crash behavior is old-or-complete-new. Deterministic fault enumeration verifies that recovery never exposes partial destination replacement, shared source/destination ownership, allocator/reference disagreement, namespace mutation, or a dirty fsck state. A second recovery after checkpointing is idempotent.

This does not change the on-disk format. Format v5 continues to store explicit inode block vectors; contiguity is only an allocation-time property of the fresh clone copies. The operation does not implement persistent extents, reflinks/COW, sparse holes, partial-block EOF semantics, or broader POSIX clone semantics.
