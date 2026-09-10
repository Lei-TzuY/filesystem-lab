# Pathname directory listing

Format v5 exposes a bounded read-only namespace enumeration surface through `list_directory_at_path`.

Before pathname resolution, the API recovers and checkpoints any older committed WAL transaction. This guarantees that directory selection and enumeration observe recovered namespace state rather than stale home metadata after a committed-but-partially-replayed transaction. The API then resolves one absolute pathname from the root with the existing bounded symbolic-link rules, including following a final symlink that names a directory. The resolved inode must be a durable directory inode. Its immediate children are read from the durable directory table and joined against the durable inode table.

Each returned `PathDirectoryEntry` contains:

- the durable entry name;
- the target inode id;
- the target inode kind (`File`, `Directory`, or `Symlink`).

Child symbolic links are reported as symbolic links rather than followed. Results are sorted by entry name so enumeration is deterministic even if directory-table record order differs.

The reader rejects namespace/inode disagreement when an entry targets an inode absent from the inode table. Existing path validation remains in force: paths must be absolute and may not use empty trailing components, `.` or `..`.

## Recovery boundary

The recovery-before-resolution ordering is covered by deterministic crash enumeration. The regression creates a directory symlink, interrupts the symlink transaction at every modeled crash point, selects reboot states whose durable journal already contains `Commit`, and calls `list_directory_at_path` through that symlink without an explicit recovery call. The listing must converge to the committed namespace, preserve allocator/inode accounting and unique physical-block ownership, pass fsck, leave an empty checkpointed journal, and make a second recovery a no-op.

## Deliberate boundary

This is not a full `readdir(3)` or FUSE adapter. It does not synthesize `.` or `..`, expose pagination cookies/offsets, persist directory ordering, or add permissions and byte-size metadata. Those surfaces can be layered later on top of this exact durable namespace read primitive.

No on-disk encoding or schema changes are introduced. Filesystem format remains v5.
