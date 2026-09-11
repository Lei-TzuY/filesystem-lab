# Pathname directory rename exchange

`rename_exchange_directories_at_path_journaled` atomically exchanges two existing directory namespace entries while keeping the filesystem format at v5.

Both parent pathnames are resolved with the repository's bounded symbolic-link traversal rules. Final components are not followed. A single terminal slash is accepted on either operand as directory intent and is stripped before the final component is split; repeated trailing separators such as `/left/a//` remain invalid. The operation still requires both named final targets themselves to be directories, so a final symbolic link is not followed merely because its pathname ends in `/`.

Before either parent is resolved, any older committed WAL is recovered and checkpointed. The operation then publishes only a new directory-table image through the existing WAL path; inode records, allocator ownership, and data blocks are preserved exactly. Before WAL publication, the complete candidate namespace is checked for directory cycles. Exchanges that would place an ancestor beneath its descendant are rejected with `InvalidInput` and do not publish a journal transaction.

Deterministic crash enumeration covers every modeled write/flush boundary through the trailing-slash pathname entry point. Recovery must converge to either the complete old namespace or the complete exchanged namespace. In either state allocator and inode images remain unchanged, `fsck` succeeds, checkpointing leaves an empty journal, and a second recovery is idempotent.

This is a pathname-semantics change only. Filesystem format remains v5 and no migration is required.
