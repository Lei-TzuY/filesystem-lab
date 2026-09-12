# Contiguous pathname clone-create

`clone_file_blocks_contiguous_to_path_journaled()` copies a non-empty range of complete logical blocks from one regular-file pathname into a fresh destination pathname while requiring all destination blocks to occupy one contiguous physical run.

Before source lookup the operation recovers and checkpoints older committed WAL, then follows the bounded symbolic-link resolver. The selected source range is snapshotted completely before destination mutation begins. Destination creation uses the existing contiguous create path: the allocator reserves the lowest-address free run large enough for the copy and allocation ownership, the new inode mapping, namespace insertion, and copied data images are published through one WAL transaction.

The source inode, namespace, block vector, and data are not modified. Destination blocks are independent physical copies and must not overlap source ownership. A crash before durable commit leaves no destination; a committed transaction recovers to the complete destination mapping and data. Recovery remains idempotent and the recovered image must pass the existing allocator, ownership, namespace, and fsck invariants.

This does **not** change the on-disk format. Format v5 still stores explicit inode block vectors. Contiguity is guaranteed only for the allocation made by this operation and is not a persistent extent invariant. The operation does not add reflink/COW, shared extents, sparse holes, partial-block EOF, or byte-length semantics.

The API rejects zero-length ranges, out-of-range source spans, malformed paths, non-file sources, destination collisions, journal-capacity exhaustion, and cases where total free space is too fragmented to provide one sufficiently large run before publication.
