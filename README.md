# filesystem-lab

A focused ELF-like systems laboratory for building and verifying a small filesystem stack from first principles, with explicit crash-consistency and corruption invariants.

## Current milestone

The repository currently establishes the block-device foundation only:

- fixed 4 KiB logical blocks;
- file-backed block device creation/opening;
- strict block-index bounds checking;
- checked device-size/offset arithmetic;
- rejection of non-block-aligned backing files;
- explicit durable flush boundary via `sync_data`;
- regression coverage for persistence, bounds, alignment, and size overflow.

No on-disk filesystem format is defined yet. The intended core progression is:

1. block layer;
2. versioned superblock/on-disk format;
3. allocation invariants;
4. inode and directory model;
5. cache/dirty-state semantics;
6. journal/WAL and deterministic crash injection;
7. recovery and fsck invariants.

Large POSIX surface-area work, FUSE integration, complex extents, permissions, and other broad features are intentionally deferred until crash semantics and the durable core are well specified and executable.
