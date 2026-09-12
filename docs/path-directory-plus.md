# Pathname directory metadata enumeration

`list_directory_with_metadata_at_path` provides a bounded read-only readdir-plus style surface over recovered filesystem format v5 state.

Before the directory pathname is resolved, the operation recovers and checkpoints any committed WAL and runs the repository-wide read-only fsck. It then joins the durable inode and directory tables from that recovered state and returns each immediate child in deterministic name order with:

- entry name;
- inode ID;
- persisted inode kind;
- logical block-vector length; and
- namespace-reference count derived from the complete durable directory table.

Child symbolic links are reported as symbolic-link inodes and are not followed during enumeration. Hard-linked files or symlinks therefore expose the same inode ID and the same global durable namespace-reference count from every directory entry that names them.

The operation does not mutate allocator, inode, namespace, journal, or data state. Filesystem format remains v5. Because v5 does not persist byte EOF, uid/gid, permissions, timestamps, or a stored link-count field, the API does not fabricate those POSIX fields.

Full fsck validation before enumeration means allocator ownership, inode references, root reachability, namespace targets, and directory-cycle invariants must already agree. A corrupt image is rejected rather than partially enumerated.
