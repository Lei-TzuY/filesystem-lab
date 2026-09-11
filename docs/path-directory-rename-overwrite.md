# Pathname directory rename-overwrite

`rename_overwrite_directory_at_path_journaled` adds a bounded `rename`-style replacement for directory endpoints while keeping filesystem format v5.

Both parent pathnames use bounded symbolic-link traversal and final components are not followed. The destination must be an empty, singly referenced, non-root directory. The source directory keeps its inode and subtree while the destination inode and any owned blocks are retired in the same allocation + inode + directory WAL transaction.

Before either parent pathname is resolved, any older committed WAL is recovered and checkpointed. Endpoint selection therefore comes from the fully recovered durable namespace rather than from a partially replayed home-write prefix. This matters when a parent symbolic link was committed before a crash but not all of its home metadata reached the device.

Before WAL publication the complete candidate namespace is checked for directory cycles. Non-empty destinations, wrong endpoint kinds, aliasing endpoints, invalid ownership, and cycle-producing moves are rejected without publishing a transaction.

Deterministic write/flush crash enumeration requires recovery to expose either the complete old namespace or the complete replacement namespace. A dedicated recovery-boundary regression also enumerates committed crash prefixes of parent-symlink creation and requires the pathname overwrite to recover that link before resolving either endpoint. Post-recovery fsck, empty-journal checkpointing, allocator/inode/namespace invariants, unique physical ownership, and second-recovery idempotence remain mandatory.
