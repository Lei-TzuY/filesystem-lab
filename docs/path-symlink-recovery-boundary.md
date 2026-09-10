# Pathname symlink recovery boundary

`create_symlink_at_path_journaled` and `unlink_symlink_at_path_journaled` recover and checkpoint any older committed WAL before resolving the destination parent pathname. This prevents parent selection from observing stale home namespace state after a crash that persisted a namespace transaction commit but not every home-location replay write.

The final component remains no-follow for unlink and remains unresolved for create collision handling; only intermediate/parent components use bounded symlink expansion.

Deterministic crash regression coverage enumerates committed parent-symlink crash states and then invokes pathname create/unlink without an explicit external recovery step. The assertions cover recovered namespace visibility, symlink payload semantics, allocator/inode accounting, unique physical-block ownership, fsck cleanliness, empty-journal checkpointing, and idempotent second recovery.

Filesystem format remains v5. This slice changes operation ordering only; it introduces no on-disk encoding or migration change.
