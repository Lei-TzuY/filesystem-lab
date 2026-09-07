# Pathname regular-file final unlink

`unlink_file_at_path_journaled` removes the final durable namespace reference to a regular file addressed by an absolute pathname.

Intermediate pathname components, including a symbolic-link parent, use the existing bounded symlink-following resolver. The final component is deliberately not followed. If it names a symbolic link, directory, or another unsupported inode kind, the operation fails before WAL publication rather than deleting the resolved target.

The selected regular-file inode must have exactly one namespace reference. Multiply linked files remain owned by the existing non-final hard-link unlink lifecycle and are rejected here. Every file block must be uniquely referenced by that inode and allocator-owned before mutation begins.

A successful final unlink frees exactly every physical block referenced by the file inode, removes that inode, removes exactly the selected directory entry, and publishes the allocation, inode-table, and directory-table images through the existing bounded unlink WAL transaction. Filesystem format remains v5; this operation adds no orphan state, byte-length semantics, sparse-hole handling, or recursive directory deletion.

Deterministic crash enumeration covers every injected write/flush boundary of the pathname operation. After recovery and checkpoint, the durable filesystem must be either the complete old state or the complete unlinked state. The recovered state must preserve allocator/inode/namespace agreement, contain no duplicate physical ownership, pass read-only fsck, leave the journal empty, and make a second recovery a no-op.
