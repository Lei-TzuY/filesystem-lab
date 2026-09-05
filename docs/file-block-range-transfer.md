# Crash-consistent regular-file logical-block range transfer

Format v5 supports a bounded metadata-only transfer of complete logical blocks between two distinct existing regular files.

`transfer_file_block_range_journaled` removes one contiguous physical-block reference range from the source inode and inserts the same references at a destination logical boundary. The allocator image is not changed, block contents are not copied, inode identities and namespace entries are preserved, and the inode-table mutation is published through one WAL transaction.

## Validation before WAL publication

The operation rejects a zero-length range, identical source/destination inode IDs, missing or non-regular-file endpoints, a source range beyond the source logical block vector, and a destination index beyond the destination logical block vector. Every moved physical block must still be allocator-owned and must not already be referenced by the destination inode.

## Crash contract

The only durable semantic change is the inode-reference assignment. Before commit, recovery preserves the old source/destination vectors. After a durable commit, the journal is authoritative until the complete inode-table home image is replayed and checkpointed. Deterministic crash enumeration covers journal publication, home replay, journal clearing, and flush boundaries; after reboot, recovery must converge to either the complete old state or the complete transferred state, never a completed double-reference or lost-reference state.

Post-recovery fsck must confirm that every allocated block has exactly one inode owner, no transferred block is double referenced, total allocated/free accounting is unchanged, both namespace entries remain valid, and a second recovery/checkpoint is a no-op.

## Deliberate limits

This is block-granular because format v5 has no persisted byte length. It does not define byte-range move semantics, same-inode reordering, file extension, EOF behavior, sparse holes, extents, reflinks, or a POSIX syscall contract. Those require separate, explicitly verified slices.
