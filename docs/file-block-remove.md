# Regular-file logical block removal

Format v5 exposes `remove_file_block_journaled` as a bounded block-granular shrink primitive. It removes exactly one existing logical block from a regular file, releases the corresponding physical block in the allocator, and shifts the remaining logical suffix left by one position.

The allocator image and inode-table image are published through the existing metadata WAL/recovery/checkpoint path. Namespace metadata is unchanged. A completed filesystem state therefore cannot retain an inode reference to a block that has already been freed, nor keep allocator ownership for the removed block after the inode reference has disappeared.

The operation validates the target inode kind, logical index, and allocator ownership before WAL publication. Missing or non-file inodes and out-of-range indexes are rejected as `InvalidInput`; allocator/reference disagreement is rejected as `InvalidData`.

Deterministic crash enumeration covers every modeled write/flush boundary in publication, home replay, and checkpoint. Before recovery, a crash image must be either the complete old state, the complete new state, or a mixed metadata prefix rejected by fsck. Recovery must converge to the committed state, clear the journal, remain fsck-clean, and a second recovery/checkpoint pass must be a no-op.

This does not change the on-disk format. Format v5 still has no persisted byte length, sparse-hole representation, or extent model, so the primitive does not claim byte-range collapse, EOF, hole-punch, or POSIX `fallocate` semantics.
