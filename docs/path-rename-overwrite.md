# Pathname rename-overwrite

Format v5 exposes pathname-facing atomic overwrite for regular files through `rename_overwrite_file_at_path_journaled` and the multiply-linked-destination variant `rename_overwrite_linked_file_at_path_journaled`.

Any older committed WAL is recovered and checkpointed before source and destination parent resolution, so both endpoint parents are selected from recovered namespace state rather than partially replayed home metadata. Only source and destination parents are then resolved with bounded symlink traversal. Final components are not followed. For a singly linked destination, namespace replacement, destination inode removal, and release of exactly its owned blocks use the existing rename-overwrite WAL transaction. For a multiply linked destination, only the selected destination alias is replaced and remaining aliases retain the inode and allocation.

The operation does not change the on-disk format. Crash tests enumerate modeled write/flush interruption points and require recovery to expose either the complete old state or complete replacement state, followed by clean fsck, empty checkpointed journal, valid allocator/inode ownership, and idempotent second recovery. A dedicated recovery-boundary regression also enumerates committed-but-not-fully-replayed parent-symlink creation states and requires pathname rename-overwrite to recover that namespace change before resolving either parent.

This is deliberately narrower than POSIX `rename(2)`: directory overwrite, arbitrary inode-kind replacement, cross-filesystem rename, permissions, timestamps, and persisted byte EOF are outside format v5's current contract.
