# Pathname logical-block removal

`remove_file_block_at_path_journaled` composes bounded absolute pathname resolution with the existing format-v5 `remove_file_block_journaled` transaction.

Before resolving the pathname, the wrapper recovers and checkpoints any older durable WAL. This makes intermediate and final symbolic links visible from the recovered namespace before target selection. Path resolution then follows the repository-wide bounded expansion rules. After resolution, the inode-ID primitive removes exactly one logical block reference, releases its physical block in allocator metadata, shifts the remaining logical suffix left, and publishes allocator and inode metadata through one WAL transaction.

The operation is intentionally block-granular. Format v5 has no persisted byte length and this API does not claim EOF, sparse-hole, byte-range collapse, `fallocate`, or extent semantics. The on-disk format remains v5.

Validation rejects missing or non-file targets and logical indices outside the existing block vector before a valid new filesystem state is accepted. Recovery/checkpoint failures, allocator ownership disagreement, journal-capacity failures, and durable I/O failures are propagated.

Durability tests enumerate every deterministic write/flush interruption of the remove transaction. After recovery, allocator and inode state must be either the complete pre-remove image or the complete committed remove image; namespace metadata must remain unchanged. A second deterministic crash matrix interrupts creation of a final symlink and, for every reboot state whose old WAL has a durable Commit record, invokes pathname removal without an external recovery step. The operation must recover that symlink before resolution, remove the requested file block, preserve unique physical ownership and allocation accounting, pass read-only fsck, checkpoint the journal to empty, and converge to a no-op on a second recovery.
