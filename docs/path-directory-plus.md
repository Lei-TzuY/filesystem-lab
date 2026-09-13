# Pathname directory metadata enumeration

`list_directory_with_metadata_at_path` provides a read-only readdir-plus style surface over recovered filesystem format v5 state.

Before the directory pathname is resolved, the operation recovers and checkpoints any committed WAL and runs the repository-wide read-only fsck. It then joins the durable inode and directory tables from that recovered state and returns each immediate child in deterministic name order with:

- entry name;
- inode ID;
- persisted inode kind;
- logical block-vector length; and
- namespace-reference count derived from the complete durable directory table.

`list_directory_with_metadata_page_at_path` exposes the same recovered and fsck-validated metadata through a bounded result page. Callers provide a positive `limit` and may provide an exclusive `after` name cursor. Only names that sort lexicographically after the cursor are eligible. When more eligible children remain, `next_after` is the final returned name and can be supplied to the next call; otherwise it is `None`.

The cursor is value-based rather than an index into the directory table. It does not have to name a currently existing child, so a caller can continue after a name that was removed between calls. Each call independently recovers and validates one durable snapshot, however, so pagination across concurrent namespace mutations is not a multi-call snapshot guarantee: newly inserted or removed names can affect later pages according to their lexical position.

A zero page limit is rejected to prevent non-advancing pagination. The implementation keeps deterministic name ordering and returns at most `limit` entries, but format v5 still stores the directory namespace as its existing table image; this API is a bounded consumer surface, not an on-disk directory-index or B-tree claim.

Child symbolic links are reported as symbolic-link inodes and are not followed during enumeration. Hard-linked files or symlinks therefore expose the same inode ID and the same global durable namespace-reference count from every directory entry that names them.

The operations do not mutate allocator, inode, namespace, journal, or data state. Filesystem format remains v5. Because v5 does not persist byte EOF, uid/gid, permissions, timestamps, or a stored link-count field, the APIs do not fabricate those POSIX fields.

Full fsck validation before enumeration means allocator ownership, inode references, root reachability, namespace targets, and directory-cycle invariants must already agree. A corrupt image is rejected rather than partially enumerated.
