# Pathname rename without overwrite

`rename_at_path_journaled` adds absolute-path addressing to the existing bounded durable rename primitive.

Both source and destination are split into parent path plus final component. Parent paths are resolved with the existing bounded symbolic-link traversal rules. Final components are not followed, so renaming a symbolic link moves the link inode itself and an existing destination remains a collision. A single terminal slash on either pathname is accepted as bounded directory-only intent: the slash is removed before splitting, repeated trailing separators remain invalid, and the named source entry must be a persisted directory inode. This does not make final symlinks followable; a symlink named with a trailing slash is rejected instead of renaming its target. A destination such as `/dst/new_dir/` therefore names a not-yet-existing directory destination only when the source entry itself is a directory.

## Durability contract

The operation changes only the directory table. Allocation ownership, inode images, and file data remain unchanged.

Deterministic write/flush fault enumeration exercises the trailing-slash directory entry point and verifies:

- recovery yields either the complete old namespace or the complete renamed namespace;
- allocator and inode images remain unchanged;
- no new or duplicate block ownership is introduced;
- read-only fsck accepts every recovered state;
- checkpointing clears the journal;
- a second recovery is idempotent.

This is intentionally narrower than full POSIX `rename(2)` pathname semantics: it does not add overwrite flags, normalize repeated separators broadly, follow final symlinks, or model mount points/open-directory lifetime. The slice changes no durable encoding; the filesystem remains format v5.
