# Pathname directory listing

Format v5 exposes bounded read-only namespace enumeration through `list_directory_at_path`, `list_directory_page_at_path`, and `list_directory_page_after_name_at_path`.

Before pathname resolution, the APIs recover and checkpoint any older committed WAL transaction. This guarantees that directory selection and enumeration observe recovered namespace state rather than stale home metadata after a committed-but-partially-replayed transaction. The APIs then resolve one absolute pathname from the root with the existing bounded symbolic-link rules, including following a final symlink that names a directory. The resolved inode must be a durable directory inode. Its immediate children are read from the durable directory table and joined against the durable inode table.

Each returned `PathDirectoryEntry` contains:

- the durable entry name;
- the target inode id;
- the target inode kind (`File`, `Directory`, or `Symlink`).

Child symbolic links are reported as symbolic links rather than followed. Results are sorted by entry name so enumeration is deterministic even if directory-table record order differs.

The reader rejects namespace/inode disagreement when an entry targets an inode absent from the inode table. Existing path validation remains in force: paths must be absolute and may not use empty repeated trailing components.

## Offset pagination

`list_directory_page_at_path(device, superblock, path, offset, limit)` applies `offset` and `limit` after the same recovery, validation, and deterministic name ordering as the complete listing. `offset` is a zero-based index into that sorted snapshot and `limit` bounds the number of returned entries. A zero limit, or an offset at or beyond the end of the snapshot, returns an empty page without integer-range arithmetic.

The offset is intentionally not a durable `readdir(3)` cookie. Each call reconstructs the current recovered namespace snapshot, so a namespace mutation between page calls may insert, remove, or rename entries before the next offset and therefore move entries across page boundaries.

## Name-cursor pagination

`list_directory_page_after_name_at_path(device, superblock, path, after_name, limit)` resumes strictly after a lexical entry-name boundary in the same deterministic ordering. `None` starts at the first child. A cursor does not need to name an existing entry; it acts as an insertion boundary, so a cursor such as `charlie` resumes at the first durable name that sorts after `charlie`.

This avoids one important weakness of positional offsets: insertion of a new entry whose name sorts before an already observed cursor does not shift the resume boundary. It is nevertheless not a durable `readdir(3)` cookie or a snapshot handle. Renames, deletions, and insertions at or after the cursor between calls can still change subsequent pages.

Both page APIs bound only the returned vector. They still read and validate the complete persisted inode and directory table images and sort the matching directory entries before applying their boundary. They therefore improve caller-facing bounded iteration without claiming indexed or tree-backed on-disk directory scaling.

## Recovery boundary

The recovery-before-resolution ordering is covered by deterministic crash enumeration. The regression creates a directory symlink, interrupts the symlink transaction at every modeled crash point, selects reboot states whose durable journal already contains `Commit`, and calls `list_directory_at_path` through that symlink without an explicit recovery call. The listing must converge to the committed namespace, preserve allocator/inode accounting and unique physical-block ownership, pass fsck, leave an empty checkpointed journal, and make a second recovery a no-op.

Both paginated APIs delegate to the same recovered and validated listing primitive before applying their boundary and limit, so they inherit that recovery boundary without introducing a new durability publication path.

## Deliberate boundary

This is still not a full `readdir(3)` or FUSE adapter. It does not synthesize `.` or `..`, persist directory ordering, provide durable pagination cookies, or add permissions and byte-size metadata. Those surfaces can be layered later on top of this exact durable namespace read primitive.

No on-disk encoding or schema changes are introduced. Filesystem format remains v5.
