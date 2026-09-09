# Pathname same-file logical-block range move

`move_file_block_range_at_path_journaled` composes bounded absolute pathname resolution with the existing format-v5 `move_file_block_range_journaled` primitive.

Before pathname resolution, the wrapper recovers and checkpoints any older committed WAL. This retry boundary prevents intermediate or final symlink lookup from deriving the target inode from partially replayed home namespace state. The pathname then follows intermediate and final symbolic links using the same expansion bound as other pathname operations. After resolution, the inode-ID primitive performs the durable mutation: remove one non-empty contiguous logical-block range from a regular file and reinsert the same physical block references at a caller-selected index in the post-removal logical-block vector.

The operation does not allocate, free, or copy data blocks. Allocator ownership, namespace entries, inode identity, physical block contents, and filesystem format stay unchanged. Only the selected inode's logical block-reference ordering advances through the existing WAL/checkpoint path.

Validation happens before the move WAL is published. The operation rejects a missing or non-file resolved inode, a zero-length or out-of-range source interval, a destination index beyond the post-removal end, a no-op destination, duplicate physical references, and allocator ownership disagreement. Recovery/checkpoint failures and path lookup failures, including dangling symlinks, are propagated.

Crash coverage enumerates every deterministic device write/flush interruption in the move itself. Recovery must produce either the complete pre-move inode table or the complete committed post-move inode table; mixed logical ordering is not accepted. Additional deterministic coverage crashes a final-symlink create at every modeled device boundary and, for states with a durable commit, invokes pathname move without external recovery. The move must first recover that symlink, resolve it, preserve unique allocator ownership, produce the requested logical ordering, pass read-only fsck, checkpoint to an empty journal, and make a second recovery pass idempotent.

Format v5 still has no persisted byte length. This API is block-granular only and does not claim byte-range move, EOF, sparse-hole, extent, reflink, or broader POSIX semantics.
