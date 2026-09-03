# filesystem-lab

A focused filesystem implementation and crash-consistency laboratory for building and verifying a small filesystem stack from first principles, with explicit persistence and corruption invariants.

## Current milestone

The repository now establishes the block-device foundation, an explicitly versioned on-disk format, executable allocation/inode/directory invariants, buffer-cache durability semantics, deterministic journal replay, a durable bounded journal, committed home-location recovery, read-only fsck, persistent allocation metadata, and crash-atomic allocator persistence through the WAL:

- fixed 4 KiB logical blocks and a file-backed block device with strict bounds/size checking;
- explicit durable flush boundary via `sync_data`;
- **format v3** superblock at block zero with total block count plus explicit journal and allocation reservations;
- deterministic metadata prefix: superblock → journal → allocation bitmap image;
- allocation image v1 with magic/version/bitmap length/CRC-32, strict zero padding, and exact geometry validation;
- one persistent ownership bit per filesystem block while reserved metadata ownership remains implicit in the superblock geometry;
- tail-block-first, header-block-last direct allocation-image writes so mixed multi-block updates are rejected by checksum validation;
- deterministic first-fit allocator with reserved-block exclusion, no-double-ownership behavior, accounting invariants, exhaustion, and double-free rejection;
- persistent allocator round-trip that reconstructs exact sparse data-block ownership;
- one bounded whole-image **journaled allocator update** path that persists the WAL before allocation home writes and supports idempotent recovery after a home-write failure;
- journal target validation that permits allocation-metadata home blocks while still forbidding superblock and journal self-overwrite;
- in-memory inode lifecycle and directory namespace models with executable ownership/reference invariants;
- cache entries with explicit `Clean`, `Dirty`, and `Writeback` states and durability-aware eviction rules;
- logical journal transactions with begin/full-block-write/commit records and deterministic crash prefixes;
- independently versioned persistent journal record codec and bounded journal-region image with CRC validation;
- committed-only recovery that validates the journal before home writes and remains idempotent across partial replay failure;
- read-only fsck over superblock, allocation metadata, journal integrity, transaction structure, reserved-region protection, and allocated/free accounting;
- focused corruption/fault regressions, including crash-before-commit allocator suppression and retry after a committed allocation home-write failure.

The current filesystem format is documented in [`docs/on-disk-format.md`](docs/on-disk-format.md). Versions 1 and 2 are retained there as historical schemas and are intentionally rejected by the version-3 reader; durable semantics are never silently reinterpreted. Journal region/record formats remain independently versioned in [`docs/journal-region-format.md`](docs/journal-region-format.md) and [`docs/journal-record-format.md`](docs/journal-record-format.md). Recovery ordering, including journaled allocation mutation, is documented in [`docs/recovery.md`](docs/recovery.md), and fsck invariants in [`docs/fsck.md`](docs/fsck.md).

Allocation is durable and can now be mutated through the bounded WAL, but the journaled path deliberately keeps the complete allocation image in one transaction. Large allocation images that do not fit the fixed journal are rejected rather than split. Persistent inode/directory formats, checkpointing, journal clearing, and circular journal head/tail metadata remain intentionally undefined.

The intended core progression is:

1. block layer;
2. versioned superblock/on-disk format;
3. allocation invariants;
4. inode lifecycle and directory model;
5. cache/dirty-state semantics;
6. journal transaction/replay semantics and deterministic crash prefixes;
7. durable journal region layout;
8. persistent journal record encoding;
9. journal-region binding;
10. recovery/home-location replay;
11. fsck/corruption invariants;
12. persistent allocation metadata;
13. journaled allocation mutation;
14. persistent inode metadata and cross-layer ownership fsck.

Large POSIX surface-area work, FUSE integration, complex extents, permissions, and other broad features are intentionally deferred until the durable metadata core and crash semantics are well specified and executable.
