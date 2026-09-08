# Pathname same-file logical-block range move

`move_file_block_range_at_path_journaled` composes bounded absolute pathname resolution with the existing format-v5 `move_file_block_range_journaled` primitive.

The pathname follows intermediate and final symbolic links using the same expansion bound as other pathname operations. After resolution, the inode-ID primitive performs the durable mutation: remove one non-empty contiguous logical-block range from a regular file and reinsert the same physical block references at a caller-selected index in the post-removal logical-block vector.

The operation does not allocate, free, or copy data blocks. Allocator ownership, namespace entries, inode identity, physical block contents, and filesystem format stay unchanged. Only the selected inode's logical block-reference ordering advances through the existing WAL/checkpoint path.

Validation happens before WAL publication. The operation rejects a missing or non-file resolved inode, a zero-length or out-of-range source interval, a destination index beyond the post-removal end, a no-op destination, duplicate physical references, and allocator ownership disagreement. Path lookup failures, including dangling symlinks, are propagated.

Crash coverage enumerates every deterministic device write/flush interruption. Recovery must produce either the complete pre-move inode table or the complete committed post-move inode table; mixed logical ordering is not accepted. The allocation image and directory table remain byte-equivalent to their pre-operation state, read-only fsck must succeed, the recovered journal checkpoint must be empty, and a second recovery pass must be idempotent.

Format v5 still has no persisted byte length. This API is block-granular only and does not claim byte-range move, EOF, sparse-hole, extent, reflink, or broader POSIX semantics.
