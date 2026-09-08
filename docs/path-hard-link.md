# Pathname hard links

Format v5 exposes two bounded pathname hard-link surfaces:

- `hard_link_file_at_path_journaled` adds one durable namespace alias to an existing regular file.
- `hard_link_symlink_at_path_journaled` adds one durable namespace alias to the final symbolic-link inode itself.

The regular-file source pathname is resolved with the existing bounded symlink-following resolver, including its final component. The symlink variant follows intermediate source symlinks but deliberately does not follow the final component, then requires that final inode to be a symbolic link. In both variants, the destination is split into parent path plus basename; only the parent path is resolved, so the final destination name is never followed and any existing entry is a collision.

Publication is delegated to the existing directory-only WAL primitives: `hard_link_file_journaled` for regular files and `hard_link_symlink_journaled` for symbolic links. The symlink primitive additionally validates the persisted one-block `SYM1` payload before WAL publication.

## Durability contract

Because format v5 derives link count from directory references, creating either kind of hard link does not change the allocator image, inode table, or file/symlink data. Only the directory table advances. A crash before durable commit must recover the old namespace. A crash after commit may interrupt home writes, but recovery must converge to the complete new namespace with both names referencing the same inode.

Deterministic write/flush fault enumeration verifies:

- old-or-complete-new namespace state;
- allocator and inode images remain unchanged;
- symlink target payload remains valid and unchanged for symbolic-link hard links;
- no new block ownership or duplicate ownership is introduced;
- read-only fsck accepts every recovered state;
- checkpointing clears the journal;
- a second recovery is idempotent.

These operations do not change the on-disk schema. The filesystem remains format v5.
