# Pathname directory rename-overwrite

`rename_overwrite_directory_at_path_journaled` adds a bounded `rename`-style replacement for directory endpoints while keeping filesystem format v5.

Both parent pathnames use bounded symbolic-link traversal and final components are not followed. The destination must be an empty, singly referenced, non-root directory. The source directory keeps its inode and subtree while the destination inode and any owned blocks are retired in the same allocation + inode + directory WAL transaction.

Before WAL publication the complete candidate namespace is checked for directory cycles. Non-empty destinations, wrong endpoint kinds, aliasing endpoints, invalid ownership, and cycle-producing moves are rejected without publishing a transaction.

Deterministic write/flush crash enumeration requires recovery to expose either the complete old namespace or the complete replacement namespace. Post-recovery fsck, empty-journal checkpointing, allocator/inode/namespace invariants, and second-recovery idempotence remain mandatory.