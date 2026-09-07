# Pathname symbolic-link unlink

`unlink_symlink_at_path_journaled` removes the final symbolic-link inode named by an absolute pathname without following that final link.

Intermediate components, including a symlinked parent, use the existing bounded pathname resolver. After resolving the parent directory, the operation delegates to the existing `unlink_symlink_journaled` transaction, so allocator release, inode removal, namespace removal, WAL publication, recovery, checkpoint clearing, and fsck invariants remain centralized.

The slice is intentionally bounded:

- absolute paths only;
- the root path and trailing-slash empty final components are rejected;
- the final component must name a singly referenced persisted symbolic-link inode;
- the symlink target is not followed;
- no recursive removal, directory removal, or generic POSIX `unlink(2)` dispatch is introduced;
- filesystem format remains v5.

Deterministic write/flush crash enumeration exercises unlink through a symlinked parent. Recovery must yield either the complete old state or the complete removed state, preserve allocator/inode/directory agreement, leave fsck clean, clear the journal, and make a second recovery a no-op.
