# Contiguous pathname clone append

`clone_file_blocks_contiguous_append_at_path_journaled()` copies a non-empty range of complete logical blocks from a pathname-resolved regular file and appends fresh independent physical copies to another pathname-resolved regular file.

The operation first recovers and checkpoints any older committed WAL, resolves source and destination with the bounded symlink-following pathname rules, and snapshots the complete source block images before mutating the destination. The fresh destination blocks are then allocated as the lowest-address first-fit contiguous run large enough for the full clone range. Allocation metadata, destination inode growth, and every copied data image are published by one WAL transaction.

Source and destination may resolve to the same inode because snapshotting occurs before append allocation or inode growth. The appended blocks never share physical ownership with the source mapping.

## Durability contract

Deterministic crash-prefix tests enumerate every modeled interruption point. After reboot and journal recovery, the device must expose either the complete pre-operation state or the complete appended clone. Tests additionally require:

- unchanged source inode mapping and source data;
- unchanged namespace entries;
- no duplicate physical block ownership;
- allocator allocated/free accounting matching inode references;
- one contiguous run for all newly appended blocks;
- copied data matching the snapshotted source range;
- a clean `fsck` result;
- an empty journal after checkpoint; and
- idempotent second recovery.

## On-disk format

This capability does not change the on-disk format. Format v5 continues to persist explicit inode block vectors. The contiguous run is an allocation-time property of this operation only; it is not a persistent extent representation and does not add reflink/COW, sparse-hole, byte-level EOF, or broader POSIX semantics.
