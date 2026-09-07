# Pathname regular-file hard links

`hard_link_file_at_path_journaled` adds one durable namespace alias to an existing regular file using absolute pathnames.

The source pathname is resolved with the existing bounded symlink-following resolver, including its final component. The destination is split into parent path plus basename; only the parent path is resolved, so the final destination name is never followed and any existing entry is a collision. Publication is delegated to the existing `hard_link_file_journaled` directory-only WAL transaction.

## Durability contract

Because format v5 derives link count from directory references, creating a hard link does not change the allocator image, inode table, or file data. Only the directory table advances. A crash before durable commit must recover the old namespace. A crash after commit may interrupt home writes, but recovery must converge to the complete new namespace with both names referencing the same inode.

Deterministic write/flush fault enumeration verifies:

- old-or-complete-new namespace state;
- allocator and inode images remain unchanged;
- no new block ownership or duplicate ownership is introduced;
- read-only fsck accepts every recovered state;
- checkpointing clears the journal;
- a second recovery is idempotent.

This operation does not change the on-disk schema. The filesystem remains format v5.
