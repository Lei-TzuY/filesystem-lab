# Pathname symbolic-link creation

Format v5 supports a bounded pathname-level symbolic-link create surface:

`create_symlink_at_path_journaled(device, superblock, destination, target)`

The destination must be absolute and must contain a non-empty final component. The operation splits the destination into parent pathname and basename, resolves the parent with the existing bounded symlink-following resolver, and delegates to `create_symlink_journaled`. The final component itself is not resolved; an existing entry is a collision.

The durable contract is unchanged from inode-ID-based symbolic-link creation: allocator ownership for every target block, the new symlink inode, the namespace entry, and all encoded target block images are published by one WAL transaction. Recovery therefore yields only the old state or the complete new state. Deterministic crash tests enumerate modeled write/flush boundaries and require unique block ownership, exact allocation accounting, namespace/inode agreement, fsck cleanliness, journal clearing, and idempotent second recovery.

One-block targets retain the existing `SYM1` payload. Longer targets use the bounded `SYM2` multi-block payload documented in [`symlinks.md`](symlinks.md). Filesystem format remains v5 and existing `SYM1` images remain readable, so no filesystem migration is required.
