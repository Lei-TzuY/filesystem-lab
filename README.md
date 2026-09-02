# filesystem-lab

A focused filesystem implementation and crash-consistency laboratory for building and verifying a small filesystem stack from first principles, with explicit persistence and corruption invariants.

## Current milestone

The repository now establishes the block-device foundation, a versioned on-disk superblock, and executable allocation invariants:

- fixed 4 KiB logical blocks;
- file-backed block device creation/opening;
- strict block-index bounds checking;
- checked device-size/offset arithmetic;
- rejection of non-block-aligned backing files;
- explicit durable flush boundary via `sync_data`;
- version-1 superblock at block zero with magic, format version, block size, and total block count;
- strict superblock validation, including reserved-byte and device-size consistency checks;
- deterministic first-fit block allocator with a reserved metadata prefix;
- executable allocation invariants for no double ownership, reserved-block exclusion, allocate/free accounting, exhaustion, and double-free rejection;
- regression coverage for persistence, bounds, alignment, size overflow, format round trips, incompatible metadata, durable formatting, and allocation lifecycle behavior.

The version-1 layout is documented in [`docs/on-disk-format.md`](docs/on-disk-format.md). Allocation state is intentionally in-memory only in this milestone; durable allocator metadata requires an explicit future format revision rather than silently reinterpreting version 1.

The intended core progression is:

1. block layer;
2. versioned superblock/on-disk format;
3. allocation invariants;
4. inode and directory model;
5. cache/dirty-state semantics;
6. journal/WAL and deterministic crash injection;
7. recovery and fsck invariants.

Large POSIX surface-area work, FUSE integration, complex extents, permissions, and other broad features are intentionally deferred until crash semantics and the durable core are well specified and executable.
