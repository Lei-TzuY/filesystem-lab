# Pathname logical-block range collapse

`collapse_file_block_range_at_path_journaled` composes bounded absolute pathname resolution with the existing format-v5 `collapse_file_block_range_journaled` primitive.

Before pathname resolution, the wrapper recovers and checkpoints any older durable WAL. This prevents inode selection from observing a partially replayed namespace, including a final symbolic link created by the previous committed transaction. The pathname then follows intermediate and final symbolic links using the repository-wide bounded resolver. After resolution, the operation removes one non-empty contiguous logical-block interval from the resolved regular file. Physical blocks in the removed interval are released from allocator ownership in the same WAL transaction as the inode block-vector update; later logical blocks shift left.

Format v5 does not persist byte length. This surface is therefore intentionally block-granular and does not claim byte-range collapse, EOF, sparse-hole, `fallocate`, extent, or general POSIX semantics.

## Durable contract

Before publication of the collapse WAL, the operation recovers/checkpoints any older committed WAL and then rejects missing or non-file targets, empty or out-of-range intervals, arithmetic overflow, and allocator ownership disagreement. A recovered older transaction may make already-committed namespace/inode/block state visible before collapse validation begins.

For every deterministic write/flush crash point in collapse itself, recovery must converge to exactly one of two valid states:

- the old allocator and inode images, with every original file block still owned and referenced; or
- the complete collapsed allocator and inode images, with every removed block free and the surviving references shifted left.

A separate recovery-boundary matrix enumerates committed crash states while creating a final symlink and invokes pathname collapse without external recovery. It verifies recovered namespace visibility, exact allocated/free accounting, unique physical block ownership, correct surviving inode references, `fsck` cleanliness, an empty journal after checkpoint, and idempotent second recovery.

The operation does not change the on-disk format; filesystem format remains v5.
