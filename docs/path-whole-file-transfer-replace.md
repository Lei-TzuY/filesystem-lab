# Pathname whole-file transfer replacement

`transfer_replace_complete_file_at_path_journaled()` moves the complete persisted logical-block vector of one existing regular file into another existing regular file while preserving both inode identities and all namespace entries.

Before pathname lookup, the operation recovers and checkpoints any older committed WAL. Both source and destination then use the normal bounded symlink-following resolver. The underlying inode-level transaction validates that the endpoints are distinct regular files, that no physical block is referenced twice across either block vector, and that every referenced block agrees with allocator ownership.

The durable state transition is published as one WAL transaction spanning the allocation image and inode table:

- the source inode becomes empty;
- the destination inode receives the source's former block vector in the same order;
- every physical block displaced from the destination is released from allocator ownership;
- source physical data blocks are neither copied nor reallocated;
- namespace and directory images do not change.

If both files are empty, the operation is a validated no-op and publishes no journal transaction. An empty source over a non-empty destination atomically clears the destination and releases its former blocks.

Format v5 remains unchanged. Whole-file scope means the complete sequence of persisted 4 KiB logical blocks because v5 has no separate byte-length field. This operation therefore does not define partial-final-block EOF, sparse holes, extents, reflink/COW, or byte-range move semantics.

Deterministic crash-prefix coverage enumerates every modeled write/flush interruption for a non-empty source replacing a non-empty destination. After reboot and recovery, allocator and inode metadata must correspond together to either the complete old state or the complete new state; mixed ownership/reference states are rejected. The directory image remains unchanged, read-only fsck must pass, the checkpointed journal must be empty, and a second recovery must be idempotent.
