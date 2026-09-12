# Contiguous pathname clone splice

`clone_file_blocks_contiguous_splice_at_path_journaled()` replaces one non-empty logical-block range in an existing destination regular file with a non-empty complete-block range copied from a distinct source regular file. The source and destination ranges may have different block counts, so the destination can grow or shrink.

Before pathname resolution, the operation recovers and checkpoints any older committed WAL. Both paths then use the existing bounded symlink-following resolver. Source block data is snapshotted before destination mutation. Fresh destination blocks are allocated through the existing contiguous replacement primitive as one lowest-address first-fit physical run, while displaced destination blocks remain owned until the fresh run is reserved.

Allocator ownership, the resized destination inode block vector, and every copied data image are published through one WAL transaction. Deterministic crash-prefix tests require recovery to produce either the complete old destination or the complete new splice, while preserving source mapping/data, namespace entries, unique block ownership, allocation accounting, fsck cleanliness, journal clearing, and idempotent second recovery.

The filesystem remains format v5. Inodes still persist explicit block vectors; contiguity is an allocation-time property only. This operation does not add persistent extent encoding, reflink/COW sharing, sparse holes, byte-length/EOF semantics, partial-block splice behavior, or broader POSIX splice semantics.
