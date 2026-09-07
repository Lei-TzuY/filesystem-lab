# Pathname symbolic-link creation

Format v5 supports a bounded pathname-level symbolic-link create surface:

`create_symlink_at_path_journaled(device, superblock, destination, target)`

The destination must be absolute and must contain a non-empty final component. The operation splits the destination into parent pathname and basename, resolves the parent with the existing bounded symlink-following resolver, and delegates to the existing `create_symlink_journaled` transaction. The final component itself is not resolved; an existing entry is a collision.

The durable contract is unchanged from inode-ID-based symbolic-link creation: allocator ownership for one target block, the new symlink inode, the namespace entry, and the encoded target block are published by one WAL transaction. Recovery therefore yields only the old state or the complete new state. Deterministic crash tests enumerate every modeled write/flush boundary and require unique block ownership, allocation accounting, namespace/inode agreement, fsck cleanliness, journal clearing, and idempotent second recovery.

This is a pathname-composition slice only. Filesystem format remains v5; target payload format remains `SYM1`; there is no migration, byte-length change, sparse-file behavior, or new extent semantics.
