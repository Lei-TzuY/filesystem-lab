# Pathname hard links

Format v5 exposes two bounded pathname hard-link surfaces:

- `hard_link_file_at_path_journaled` adds one durable namespace alias to an existing regular file.
- `hard_link_symlink_at_path_journaled` adds one durable namespace alias to the final symbolic-link inode itself.

Before resolving either source or destination parent, both pathname APIs recover and checkpoint any older committed WAL. This prevents endpoint selection from observing a partially replayed namespace after reboot. The regular-file source pathname is then resolved with the existing bounded symlink-following resolver, including its final component. The symlink variant follows intermediate source symlinks but deliberately does not follow the final component, then requires that final inode to be a symbolic link. In both variants, the destination is split into parent path plus basename; only the parent path is resolved, so the final destination name is never followed and any existing entry is a collision.

Publication is delegated to the existing directory-only WAL primitives: `hard_link_file_journaled` for regular files and `hard_link_symlink_journaled` for symbolic links. The symlink primitive additionally validates the persisted one-block `SYM1` payload before WAL publication.

## Durability contract

Because format v5 derives link count from directory references, creating either kind of hard link does not change the allocator image, inode table, or file/symlink data. A crash before durable commit must recover the old namespace. A crash after commit may interrupt home writes, but recovery must converge to the complete new namespace with both names referencing the same inode.

Deterministic write/flush fault enumeration verifies:

- old-or-complete-new namespace state;
- allocator and inode images remain unchanged by the hard-link transaction itself;
- symlink target payload remains valid and unchanged for symbolic-link hard links;
- no new block ownership or duplicate ownership is introduced by the hard-link transaction;
- read-only fsck accepts every recovered state;
- checkpointing clears the journal;
- a second recovery is idempotent.

A separate recovery-boundary matrix enumerates crashes while publishing an older source or destination-parent symlink, selects reboot states whose durable WAL already contains Commit, and invokes the pathname hard-link API without an external recovery call. Those states must first recover the namespace and only then resolve the hard-link operands. The regression verifies recovered alias visibility, unchanged hard-link allocator/inode ownership, unique surviving physical references, fsck cleanliness, an empty checkpointed journal, and idempotent second recovery.

These operations do not change the on-disk schema. The filesystem remains format v5.
