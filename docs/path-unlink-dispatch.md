# POSIX-like pathname unlink dispatch

`unlink_at_path_journaled` provides one bounded pathname-facing unlink surface over the existing format-v5 unlink lifecycles.

The operation recovers and checkpoints any older committed WAL before resolving the parent pathname. Intermediate symbolic links are followed by the existing bounded resolver, but the final component is treated as a namespace key and is never followed. This matches the essential `unlink` rule that removing a symbolic-link pathname removes the link itself rather than its target.

After recovery, the operation derives the final entry's target inode kind and authoritative namespace reference count from persisted metadata:

- a multiply referenced regular file removes exactly the selected directory entry through the existing directory-only non-final hard-link transaction;
- a singly referenced regular file uses the existing final unlink transaction, releasing its owned data blocks and removing the inode;
- a multiply referenced symbolic link removes exactly the selected directory entry while preserving the symlink inode, payload blocks, and allocator ownership;
- a singly referenced symbolic link uses the existing final symlink unlink transaction, releasing its payload blocks and inode;
- directories are rejected and remain owned by the explicit directory-removal surface.

Format v5 does not persist a link-count field; link count remains derived from directory-table references. This slice does not alter the superblock, allocation image, inode record/table, directory entry/table, symlink payload, or WAL encoding, so filesystem format remains **v5** and no migration is required.

## Crash contract

The dispatcher adds no new WAL encoding and delegates publication to already bounded transactions. Its integration tests nevertheless enumerate every modeled write/flush crash boundary for all four executable branches: final regular file, non-final regular-file hard link, final symbolic link, and non-final symbolic-link hard link. After reboot and recovery, durable metadata must equal either the complete old state or the complete new state; partial allocator/inode/namespace combinations are rejected. Each recovered state must pass `fsck`, leave an empty checkpointed journal, and make a second recovery a no-op.

## Deliberate bounds

This is not a general recursive-removal or open-file/orphan implementation. It does not remove directories, does not add persisted link counts, does not model open file descriptors, and does not introduce orphan-list semantics. A trailing slash remains invalid for this non-directory unlink surface.
