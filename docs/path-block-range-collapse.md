# Pathname logical-block range collapse

`collapse_file_block_range_at_path_journaled` composes bounded absolute pathname resolution with the existing format-v5 `collapse_file_block_range_journaled` primitive.

The pathname follows intermediate and final symbolic links using the repository-wide bounded resolver. After resolution, the operation removes one non-empty contiguous logical-block interval from the resolved regular file. Physical blocks in the removed interval are released from allocator ownership in the same WAL transaction as the inode block-vector update; later logical blocks shift left.

Format v5 does not persist byte length. This surface is therefore intentionally block-granular and does not claim byte-range collapse, EOF, sparse-hole, `fallocate`, extent, or general POSIX semantics.

## Durable contract

Before WAL publication the operation rejects missing or non-file targets, empty or out-of-range intervals, arithmetic overflow, and allocator ownership disagreement. Rejected requests must leave allocator, inode table, namespace, and journal unchanged.

For every deterministic write/flush crash point, recovery must converge to exactly one of two valid states:

- the old allocator and inode images, with every original file block still owned and referenced; or
- the complete collapsed allocator and inode images, with every removed block free and the surviving references shifted left.

The directory table is unchanged in both states. After recovery, `fsck` must accept the filesystem, the journal checkpoint must be empty, and a second recovery pass must be idempotent.

The operation does not change the on-disk format; filesystem format remains v5.
