# Regular-file hard links

Format v5 permits multiple directory entries to target the same regular-file inode. `hard_link_file_journaled` exposes that capability as one bounded crash-consistent namespace operation.

The operation validates that the parent exists and is a directory, the target exists and is a regular file, and the destination `(parent, name)` is unused before publishing WAL state. It changes only the directory-table image; allocator ownership and inode block references are unchanged. Directory hard links are rejected so root reachability and directory-cycle invariants are not weakened.

Link count is not a persisted inode field in format v5. The authoritative count is therefore the number of durable directory entries targeting the regular-file inode. Existing unlink semantics still reject removal of a multiply referenced inode; removing one of several links and freeing the inode on the last link are separate lifecycle work.

The deterministic crash regression enumerates every modeled write/flush mutation boundary. After reboot, fsck must accept either the old one-name namespace or the complete two-name namespace. A durable commit is replayed to the complete two-name state, checkpoint clears the fixed journal reservation, and a second recovery/checkpoint pass is a no-op.
