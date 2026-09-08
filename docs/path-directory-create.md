# Pathname directory creation

Format v5 exposes a bounded crash-consistent pathname operation for creating one empty directory.

`create_directory_at_path_journaled` accepts an absolute destination pathname. Its parent is resolved through the existing bounded symbolic-link resolver; the final component is not resolved and becomes a new durable namespace entry. The operation allocates a fresh inode identifier, creates a `Directory` inode with an empty logical-block vector, and publishes the inode table plus directory table through the existing create WAL transaction.

Because an empty format-v5 directory owns no data blocks, the allocator image is passed through unchanged. Recovery must therefore produce exactly one of two states: the old namespace with no new inode, or the complete new directory inode and namespace entry. An inode-only or directory-entry-only state is invalid. Deterministic write/flush crash enumeration verifies old-or-new recovery, exact allocator preservation, unique block ownership, valid inode references, namespace reachability, read-only fsck cleanliness, empty checkpointed journal state, and idempotent second recovery.

The operation rejects malformed or non-absolute paths, root/trailing-slash destinations, missing or non-directory parents, namespace collisions, invalid entry names, duplicate persisted inode identifiers, and exhausted inode identifiers before publishing a new transaction.

This is intentionally narrower than POSIX `mkdir(2)`: format v5 does not persist mode bits, uid/gid, timestamps, ACLs, link counts, or `.`/`..` records. No on-disk format change is introduced; filesystem format remains v5.
