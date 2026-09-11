# Pathname hard links

Format v5 exposes three bounded pathname hard-link surfaces:

- `hard_link_at_path_journaled` is the POSIX-like default: it follows intermediate source symbolic links but does **not** follow the final source component, then dispatches by persisted inode kind to regular-file or symbolic-link hard-link publication.
- `hard_link_file_at_path_journaled` adds one durable namespace alias to an existing regular file and follows the final source symbolic link.
- `hard_link_symlink_at_path_journaled` adds one durable namespace alias to the final symbolic-link inode itself.

Before resolving either source or destination parent, all pathname hard-link APIs recover and checkpoint any older committed WAL. This prevents endpoint selection from observing a partially replayed namespace after reboot. For `hard_link_at_path_journaled`, the final source inode is selected without following a final symlink, loaded from the persisted inode table, and dispatched to the existing regular-file or symbolic-link primitive. Directory sources are rejected so directory-parent and cycle invariants remain unchanged. The destination is split into parent path plus basename; only the parent path is resolved, so the final destination name is never followed and any existing entry is a collision.

Publication remains delegated to the existing directory-only WAL primitives: `hard_link_file_journaled` for regular files and `hard_link_symlink_journaled` for symbolic links. The symlink primitive additionally validates the persisted one-block `SYM1` payload before WAL publication.

## Durability contract

Because format v5 derives link count from directory references, creating either kind of hard link does not change the allocator image, inode table, or file/symlink data. A crash before durable commit must recover the old namespace. A crash after commit may interrupt home writes, but recovery must converge to the complete new namespace with both names referencing the same inode.

Deterministic write/flush fault enumeration covers both branches of the generic pathname dispatch and verifies:

- old-or-complete-new namespace state;
- allocator and inode images remain unchanged by the hard-link transaction itself;
- symlink target payload remains valid and unchanged for symbolic-link hard links;
- no new block ownership or duplicate ownership is introduced by the hard-link transaction;
- read-only fsck accepts every recovered state;
- checkpointing clears the journal;
- a second recovery is idempotent.

A separate recovery-boundary matrix continues to enumerate crashes while publishing an older source or destination-parent symlink, selects reboot states whose durable WAL already contains Commit, and invokes the pathname hard-link API without an external recovery call. Those states must first recover the namespace and only then resolve the hard-link operands. The regression verifies recovered alias visibility, unchanged hard-link allocator/inode ownership, unique surviving physical references, fsck cleanliness, an empty checkpointed journal, and idempotent second recovery.

These operations do not change the on-disk schema. The filesystem remains format v5. The generic surface does not add directory hard links, persisted link counts, current-working-directory relative paths, or an `AT_SYMLINK_FOLLOW` option.
