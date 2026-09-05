# Regular-file logical block range collapse

Format v5 exposes `collapse_file_block_range_journaled` as a bounded block-granular shrink primitive. It removes a non-empty contiguous logical-block range from an existing regular file, releases exactly those physical blocks from allocator ownership, and shifts the surviving logical suffix left in one WAL transaction.

The inode identity and directory namespace are unchanged. Before publication, the implementation validates the complete range and confirms allocator ownership for every selected block. The allocator image and inode table are then advanced together through the existing metadata WAL, recovery, home replay, and checkpoint path.

This operation deliberately does **not** define byte-length, EOF, sparse-hole, POSIX `fallocate(FALLOC_FL_COLLAPSE_RANGE)`, or extent semantics. Format v5 still models regular-file size only as an ordered vector of complete 4 KiB block references, so callers must treat this as a logical-block primitive.

## Crash contract

Deterministic crash enumeration covers every modeled `write_block` and `flush` boundary during journal publication, allocator/inode home replay, journal clearing, and checkpoint durability. After reboot, raw metadata may be the complete old state, the complete collapsed state, or a mixed allocator/inode prefix that read-only fsck must reject. Recovery must converge to either the old state when no commit became durable or the complete collapsed state when the commit did become durable.

After successful recovery:

- every surviving inode reference is allocator-owned;
- every removed block is allocator-free;
- no block is double-owned;
- the namespace still resolves the same inode;
- fsck succeeds;
- the journal is empty; and
- a second recovery/checkpoint pass is a no-op.
