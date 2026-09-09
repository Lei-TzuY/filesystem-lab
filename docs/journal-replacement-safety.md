# Journal replacement safety

Filesystem format v5 uses one bounded persistent journal reservation. A non-empty journal image can be the only durable recovery source for a transaction whose commit reached storage before all home-location writes became durable.

`store_journal_image()` therefore refuses to publish a new non-empty image while any older non-empty image is still present. The operation returns `WouldBlock` without modifying the journal reservation. Direct lower-level callers must run recovery and checkpoint the existing image, then recompute and retry the filesystem mutation from the recovered home state.

This ordering matters because recovering only at the moment of journal replacement would be too late: a higher-level mutation may already have derived allocator, inode, directory, or data updates from stale home blocks. Refusing publication preserves the prior WAL and forces any retry boundary to start from recovered state.

The pathname create surface provides that retry boundary for new files and directories. `create_empty_file_at_path_journaled()`, `create_one_block_file_at_path_journaled()`, and `create_directory_at_path_journaled()` first validate the destination pathname shape, then recover and checkpoint any older journal before resolving the parent or loading allocator, inode, and directory state. The new mutation is therefore computed only after the prior committed state has been replayed to durable home locations.

The pathname regular-file unlink surface applies the same rule. `unlink_file_at_path_journaled()` validates the path shape, recovers and checkpoints any older durable WAL, and only then resolves the parent and loads allocator, inode, and directory state. A file created by an older committed transaction can therefore be safely unlinked after reboot even when the create crashed during home replay; unlink never derives its lifecycle mutation from the partial home-write prefix.

Pathname empty-directory removal applies the same retry boundary. `remove_directory_at_path_journaled()` validates the pathname shape, recovers and checkpoints any older durable WAL, and only then resolves the parent and evaluates directory emptiness, namespace references, inode state, and allocator ownership. A directory created by an older committed transaction can therefore be removed after reboot without deriving the rmdir mutation from a partial home-write prefix.

Pathname rename now uses the same ordering. `rename_at_path_journaled()` validates both pathname shapes, recovers and checkpoints any older durable WAL, and only then resolves source and destination parents and derives the directory-only rename mutation. A namespace entry published by an older committed transaction can therefore be renamed immediately after reboot without resolving or validating the rename against a partial home-write prefix.

The rule does not change the v5 on-disk encoding. Empty-image writes remain available for explicit journal initialization/checkpoint behavior, and malformed existing journal images are rejected by normal journal decoding before a replacement can proceed.

## Durability invariant

For any attempted second transaction while an older journal image remains durable, exactly one of these states is permitted:

1. the old journal remains authoritative and a direct low-level replacement is not published; or
2. a recovery-aware high-level operation recovers and checkpoints the old transaction, recomputes the second mutation from recovered home state, and only then publishes its new WAL.

A later transaction must never destroy the only durable copy of an earlier committed transaction, and a later mutation must never be derived from a partial post-crash home-write prefix.

## Deterministic crash coverage

`tests/journal_replacement_safety.rs` enumerates every modeled write/flush crash point of a one-block pathname create. For each reboot state whose journal contains a durable commit, the test first proves that direct WAL replacement returns `WouldBlock` and leaves the old journal byte-for-byte intact. It then invokes a second pathname create without an external recovery step.

The second create must recover and checkpoint the first transaction before recomputing allocator, inode, and namespace state. After it commits, both files must be reachable with distinct inode identities and unique physical-block ownership, allocator accounting must remain valid, fsck must pass, the journal must be empty, and a second recovery must perform no work.

`tests/path_file_unlink_recovery_boundary.rs` performs the complementary lifecycle check. It enumerates the same create crash matrix, selects every reboot state with a durable commit, proves the old journal cannot be replaced directly, then invokes pathname unlink without an external recovery call. The unlink must first recover the created file, recompute from that recovered namespace and ownership state, remove the file completely, return allocator accounting to the pre-create baseline, leave fsck clean and the journal empty, and make a subsequent recovery a no-op.

`tests/path_rmdir_recovery_boundary.rs` applies the lifecycle check to directories. It enumerates pathname mkdir crash points, selects reboot states with a durable commit, proves the committed mkdir WAL cannot be replaced directly, then invokes pathname rmdir without an external recovery call. Rmdir must recover the created directory before recomputing its namespace and inode retirement, leave only the root namespace/inode state, pass fsck, checkpoint the journal to empty, and make a subsequent recovery a no-op.

`tests/path_rename_recovery_boundary.rs` applies the lifecycle check to directory-only rename. It enumerates one-block pathname create crash points, selects reboot states with a durable commit, proves the create WAL cannot be replaced directly, then invokes pathname rename without an external recovery call. Rename must recover the created file before resolving the source and destination parents, preserve the file inode and its unique physical-block ownership, move exactly one namespace entry, preserve allocator accounting, pass fsck, checkpoint the journal to empty, and make a subsequent recovery a no-op.
