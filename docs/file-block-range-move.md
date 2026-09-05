# Same-file logical-block range move

Format v5 supports a bounded crash-consistent primitive for reordering one contiguous logical-block range inside an existing regular file.

`move_file_block_range_journaled` removes `block_count` references beginning at `source_index` and reinserts the exact physical-block references at `destination_index`. The destination index is interpreted against the logical-block vector after removal, which makes overlapping moves deterministic. A destination equal to the source index is rejected as a no-op.

The operation does not allocate, free, copy, or rewrite data blocks. Allocator ownership and total allocated/free accounting remain unchanged. Namespace state and inode identity remain unchanged. Before WAL publication the implementation validates the regular-file inode, source range, post-removal destination boundary, unique physical references, and allocator ownership of every referenced block.

Only the changed inode-table image is published through the existing WAL and recovery/checkpoint path. Deterministic crash enumeration covers every modeled write/flush mutation boundary and requires the durable inode ordering to be either the complete old order or the complete new order. Recovery must converge to the committed ordering, fsck must remain clean, the journal must clear, and a second recovery/checkpoint must be a no-op.

This remains a block-granular format-v5 operation. It does not add persisted byte length, EOF semantics, sparse holes, extents, reflinks, byte-range moves, or POSIX compatibility, and it does not change the on-disk format version.
