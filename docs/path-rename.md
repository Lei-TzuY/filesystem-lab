# Pathname rename without overwrite

`rename_at_path_journaled` adds absolute-path addressing to the existing bounded durable rename primitive.

Both source and destination are split into parent path plus final component. Parent paths are resolved with the existing bounded symbolic-link traversal rules. Final components are not followed, so renaming a symbolic link moves the link inode itself and an existing destination remains a collision. Publication is delegated to `rename_entry_journaled` and therefore retains its directory-cycle validation and directory-only WAL transaction.

## Durability contract

The operation changes only the directory table. Allocation ownership, inode images, and file data remain unchanged.

Deterministic write/flush fault enumeration verifies:

- recovery yields either the complete old namespace or the complete renamed namespace;
- allocator and inode images remain unchanged;
- no new or duplicate block ownership is introduced;
- read-only fsck accepts every recovered state;
- checkpointing clears the journal;
- a second recovery is idempotent.

This operation does not add overwrite or exchange flags and does not change the on-disk schema. The filesystem remains format v5.
