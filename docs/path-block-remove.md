# Pathname logical-block removal

`remove_file_block_at_path_journaled` composes bounded absolute pathname resolution with the existing format-v5 `remove_file_block_journaled` transaction.

Intermediate and final symbolic links are followed using the repository-wide bounded expansion rules. After resolution, the inode-ID primitive removes exactly one logical block reference, releases its physical block in allocator metadata, shifts the remaining logical suffix left, and publishes allocator and inode metadata through one WAL transaction.

The operation is intentionally block-granular. Format v5 has no persisted byte length and this API does not claim EOF, sparse-hole, byte-range collapse, `fallocate`, or extent semantics. The on-disk format remains v5.

Validation rejects missing or non-file targets and logical indices outside the existing block vector before a valid new filesystem state is accepted. Allocator ownership disagreement, journal-capacity failures, and durable I/O failures are propagated.

Durability tests enumerate every deterministic write/flush interruption. After recovery, allocator and inode state must be either the complete pre-remove image or the complete committed remove image; namespace metadata must remain unchanged. Every recovered state must pass read-only fsck, leave the journal checkpoint empty, preserve unique block ownership/allocation accounting, and converge to a no-op on a second recovery.
