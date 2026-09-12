# Contiguous pathname block insertion

`insert_file_blocks_contiguous_at_path_journaled` inserts one non-empty sequence of complete logical blocks at an existing regular-file logical boundary while requiring every newly allocated physical block to come from one lowest-address contiguous free run.

The pathname wrapper recovers and checkpoints any older committed WAL before resolving the path. Intermediate and final symbolic links use the repository's bounded symlink-following resolver. The inode-level operation then validates the regular-file target and insertion boundary before allocating the run.

Allocator ownership, the inode's explicit block-vector splice, and all inserted data images are published through one WAL transaction. Existing block references and namespace entries do not change. A crash before durable commit leaves the old state; after commit, recovery converges to the complete inserted state. Deterministic crash-prefix tests require old-or-complete-new recovery, unique block ownership, allocation accounting, clean fsck, journal checkpointing, and idempotent second recovery.

This does not change filesystem format v5. The inode format still stores explicit physical block vectors, not extent records. Contiguity is therefore an allocation-time guarantee of this operation only. The API does not define byte-level EOF, sparse holes, persistent extent allocation, or POSIX byte-range insertion semantics.
