# filesystem-lab

A focused filesystem implementation and crash-consistency laboratory for building and verifying a small filesystem stack from first principles, with explicit persistence and corruption invariants.

## Current milestone

The repository now establishes the block-device foundation and a versioned on-disk superblock:

- fixed 4 KiB logical blocks;
- file-backed block device creation/opening;
- strict block-index bounds checking;
- checked device-size/offset arithmetic;
- rejection of non-block-aligned backing files;
- explicit durable flush boundary via `sync_data`;
- version-1 superblock at block zero with magic, format version, block size, and total block count;
- strict superblock validation, including reserved-byte and device-size consistency checks;
- regression coverage for persistence, bounds, alignment, size overflow, format round trips, incompatible metadata, and durable formatting.

The version-1 layout is documented in [`docs/on-disk-format.md`](docs/on-disk-format.md). The intended core progression is:

1. block layer;
2. versioned superblock/on-disk format;
3. allocation invariants;
4. inode and directory model;
5. cache/dirty-state semantics;
6. journal/WAL and deterministic crash injection;
7. recovery and fsck invariants.

Large POSIX surface-area work, FUSE integration, complex extents, permissions, and other broad features are intentionally deferred until crash semantics and the durable core are well specified and executable.
