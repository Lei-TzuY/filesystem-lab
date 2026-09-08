# Pathname logical-block range insertion

`insert_file_blocks_at_path_journaled` composes bounded absolute pathname resolution with the existing format-v5 `insert_file_blocks_journaled` transaction.

Intermediate and final symbolic links are followed using the repository-wide bounded expansion rules. After resolution, the inode-ID primitive allocates one fresh physical block for each supplied 4 KiB image, inserts the new references at the requested logical boundary, and publishes allocator metadata, inode metadata, and the new data blocks through one WAL transaction.

The operation is intentionally block-granular. Format v5 has no persisted byte length and this API does not claim EOF, sparse-hole, byte-range insert, `fallocate`, or extent semantics. The on-disk format remains v5.

Validation rejects missing or non-file targets, empty insertions, logical indices beyond the current block count, insufficient free space, and insufficient journal capacity before a valid new filesystem state is accepted.

Durability tests enumerate every deterministic write/flush interruption. After recovery, allocator and inode state must be either the complete pre-insert image or the complete committed insert image; namespace metadata must remain unchanged. Every recovered state must pass read-only fsck, leave the journal checkpoint empty, preserve unique block ownership/allocation accounting, and converge to a no-op on a second recovery.
