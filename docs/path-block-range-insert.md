# Pathname logical-block range insertion

`insert_file_blocks_at_path_journaled` composes bounded absolute pathname resolution with the existing format-v5 `insert_file_blocks_journaled` transaction.

Before pathname resolution, the wrapper recovers and checkpoints any older durable WAL. This ensures intermediate and final symbolic links are resolved from the recovered namespace rather than a partially replayed home image. Path resolution then follows symbolic links using the repository-wide bounded expansion rules. After resolution, the inode-ID primitive allocates one fresh physical block for each supplied 4 KiB image, inserts the new references at the requested logical boundary, and publishes allocator metadata, inode metadata, and the new data blocks through one WAL transaction.

The operation is intentionally block-granular. Format v5 has no persisted byte length and this API does not claim EOF, sparse-hole, byte-range insert, `fallocate`, or extent semantics. The on-disk format remains v5.

Validation rejects missing or non-file targets, empty insertions, logical indices beyond the current block count, insufficient free space, and insufficient journal capacity before a valid new filesystem state is accepted.

Durability tests enumerate every deterministic write/flush interruption of the insertion transaction. After recovery, allocator and inode state must be either the complete pre-insert image or the complete committed insert image; namespace metadata must remain unchanged. A separate committed-symlink crash enumeration verifies that a durable-but-partially-replayed final symlink is recovered before pathname resolution and that the subsequent insertion preserves logical ordering, allocator accounting, and unique block ownership. Every recovered state must pass read-only fsck, leave the journal checkpoint empty after checkpointing, and converge to a no-op on a second recovery.
