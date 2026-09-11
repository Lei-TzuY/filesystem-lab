# Pathname hard links

Format v5 exposes one explicit source-follow policy dispatcher plus four convenience pathname hard-link surfaces:

- `hard_link_at_path_with_source_follow_journaled` accepts `HardLinkSourceFollow::{NoFollowFinal, FollowFinal}` so callers that model `link`/`linkat` semantics can select final-source behavior without choosing an inode-kind-specific API.
- `hard_link_at_path_journaled` is the POSIX-like `link` convenience surface: it follows intermediate source symbolic links but does **not** follow the final source component.
- `hard_link_following_source_at_path_journaled` is the bounded `linkat(..., AT_SYMLINK_FOLLOW)` convenience surface: it follows the complete source pathname, including the final symbolic link. A final symlink to a regular file therefore creates an alias of the regular file rather than of the symlink inode.
- `hard_link_file_at_path_journaled` adds one durable namespace alias to an existing regular file and follows the final source symbolic link.
- `hard_link_symlink_at_path_journaled` adds one durable namespace alias to the final symbolic-link inode itself.

Before resolving either source or destination parent, the policy dispatcher recovers and checkpoints any older committed WAL. `NoFollowFinal` selects the final source inode without following a final symlink; `FollowFinal` uses the bounded full-path resolver. The persisted selected inode kind then dispatches to the existing regular-file or symbolic-link primitive. Both policies reject directory sources so directory-parent and cycle invariants remain unchanged. The destination is split into parent path plus basename; only the parent path is resolved, so the final destination name is never followed and any existing entry is a collision. The two generic convenience surfaces delegate to this single policy dispatcher, keeping recovery ordering and endpoint selection identical.

Publication remains delegated to the existing directory-only WAL primitives: `hard_link_file_journaled` for regular files and `hard_link_symlink_journaled` for symbolic links. The symlink primitive additionally validates the persisted one-block `SYM1` payload before WAL publication.

## Durability contract

Because format v5 derives link count from directory references, creating either kind of hard link does not change the allocator image, inode table, or file/symlink data. A crash before durable commit must recover the old namespace. A crash after commit may interrupt home writes, but recovery must converge to the complete new namespace with both names referencing the same inode.

Deterministic write/flush fault enumeration covers both branches of the default generic pathname dispatch and the final-source-symlink-follow path to a regular file. Recovered states verify:

- old-or-complete-new namespace state;
- allocator and inode images remain unchanged by the hard-link transaction itself;
- symlink target payload remains valid and unchanged for symbolic-link hard links;
- no new block ownership or duplicate ownership is introduced by the hard-link transaction;
- read-only fsck accepts every recovered state;
- checkpointing clears the journal;
- a second recovery is idempotent.

The final-follow regression also verifies that a final source symlink resolving to a directory is rejected before WAL publication. This preserves the filesystem invariant that directories do not acquire hard-link aliases through pathname hard-link APIs.

A separate recovery-boundary matrix continues to enumerate crashes while publishing an older source or destination-parent symlink, selects reboot states whose durable WAL already contains Commit, and invokes the pathname hard-link API without an external recovery call. Those states must first recover the namespace and only then resolve the hard-link operands. The regression verifies recovered alias visibility, unchanged hard-link allocator/inode ownership, unique surviving physical references, fsck cleanliness, an empty checkpointed journal, and idempotent second recovery.

These operations do not change the on-disk schema. The filesystem remains format v5. The pathname hard-link surfaces do not add directory hard links, persisted link counts, or current-working-directory relative paths.
