# Hard links

Format v5 permits multiple directory entries to target the same regular-file inode or symbolic-link inode. `hard_link_file_journaled` exposes the regular-file operation; `hard_link_symlink_journaled` exposes the symbolic-link operation. Directory hard links remain rejected.

Both operations validate that the parent exists and is a directory, the target has the required inode kind, and the destination `(parent, name)` is unused before publishing WAL state. The symlink variant additionally validates the existing one-block `SYM1` payload through `read_symlink` before publication. A successful hard link changes only the directory-table image: allocator ownership, inode block references, and file/symlink data remain unchanged.

Link count is not a persisted inode field in format v5. The authoritative count is therefore the number of durable directory entries targeting an inode. Regular files have a separate non-final unlink lifecycle. This slice does not add non-final symlink unlink; the existing symlink final-unlink operation continues to require exactly one namespace reference.

The deterministic crash regressions enumerate every modeled write/flush mutation boundary. After reboot, fsck must accept either the old one-name namespace or the complete two-name namespace. A durable commit is replayed to the complete two-name state, checkpoint clears the fixed journal reservation, and a second recovery/checkpoint pass is a no-op. The symlink regression also requires allocator state, inode state, and the validated target payload to remain byte-semantically unchanged across the namespace-only transaction.
