# Pathname empty-directory removal

`remove_directory_at_path_journaled` adds a bounded `rmdir`-style namespace mutation for filesystem format v5.

The caller supplies an absolute pathname. Intermediate components, including the parent itself, are resolved through the existing bounded symbolic-link resolver. The final component is deliberately not followed: a final symlink is rejected instead of removing the directory it references.

The selected target must be a non-root `Directory` inode with exactly one namespace reference and no directory-table entries whose `parent` is that inode. The implementation validates any inode block references as unique and allocator-owned, frees exactly those blocks, then removes the inode and its namespace entry through the existing validated unlink transaction. Newly created directories are blockless, so the ordinary create/remove lifecycle preserves allocator ownership exactly.

Publication remains one bounded WAL transaction covering allocation, inode-table, and directory-table home state. Deterministic crash enumeration interrupts every modeled write/flush point and requires recovery to yield either the complete old namespace or the complete removed-directory state, never an inode-only or namespace-only intermediate state. The regression also checks allocator equality for blockless directories, unique block ownership, `fsck`, empty checkpointed journal state, and idempotent second recovery.

This slice does not add recursive removal, directory hard links, `.` or `..` entries, open-directory lifetime/orphan semantics, permissions, mount integration, or a new on-disk format. Filesystem format remains v5.
