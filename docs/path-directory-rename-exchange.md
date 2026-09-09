# Pathname directory rename exchange

`rename_exchange_directories_at_path_journaled` atomically exchanges two existing directory namespace entries while keeping the filesystem format at v5.

Both parent pathnames are resolved with the repository's bounded symbolic-link traversal rules. Final components are not followed. The operation requires both final targets to be directories and publishes only a new directory-table image through the existing WAL path; inode records, allocator ownership, and data blocks are preserved exactly.

Before WAL publication, the complete candidate namespace is checked for directory cycles. Exchanges that would place an ancestor beneath its descendant are rejected with `InvalidInput` and do not publish a journal transaction.

Deterministic crash enumeration covers every modeled write/flush boundary. Recovery must converge to either the complete old namespace or the complete exchanged namespace. In either state allocator and inode images remain unchanged, `fsck` succeeds, checkpointing leaves an empty journal, and a second recovery is idempotent.
