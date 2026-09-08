# Pathname directory listing

Format v5 exposes a bounded read-only namespace enumeration surface through `list_directory_at_path`.

The API resolves one absolute pathname from the root with the existing bounded symbolic-link rules, including following a final symlink that names a directory. The resolved inode must be a durable directory inode. Its immediate children are read from the durable directory table and joined against the durable inode table.

Each returned `PathDirectoryEntry` contains:

- the durable entry name;
- the target inode id;
- the target inode kind (`File`, `Directory`, or `Symlink`).

Child symbolic links are reported as symbolic links rather than followed. Results are sorted by entry name so enumeration is deterministic even if directory-table record order differs.

The reader rejects namespace/inode disagreement when an entry targets an inode absent from the inode table. Existing path validation remains in force: paths must be absolute and may not use empty trailing components, `.` or `..`.

## Deliberate boundary

This is not a full `readdir(3)` or FUSE adapter. It does not synthesize `.` or `..`, expose pagination cookies/offsets, persist directory ordering, or add permissions and byte-size metadata. Those surfaces can be layered later on top of this exact durable namespace read primitive.

No on-disk format, WAL ordering, recovery rule, allocator ownership rule, or fsck invariant changes in this slice. Filesystem format remains v5.
