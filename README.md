# filesystem-lab

A focused filesystem implementation and crash-consistency laboratory for building and verifying a small filesystem stack from first principles, with explicit persistence and corruption invariants.

## Current milestone

The repository now establishes the block-device foundation, a versioned on-disk superblock, executable allocation invariants, in-memory inode/directory models, and an explicit buffer-cache durability state machine:

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
- deterministic in-memory inode identifiers and file/directory kinds;
- explicit inode-to-block ownership with duplicate-owner rejection, reserved/free-block rejection, and detach-before-remove lifecycle rules;
- executable cross-checks that every inode block reference is allocated and agrees with the reverse ownership index;
- deterministic directory entries keyed by parent inode and path-component name;
- directory operation checks for live directory parents, live inode targets, unique names, and valid single path components;
- executable namespace validation that rejects non-directory parents and dangling inode references;
- cache entries with explicit `Clean`, `Dirty`, and `Writeback` states;
- write-back semantics that distinguish an issued device write from a completed durability boundary;
- eviction rejection for dirty or writeback entries so non-durable data cannot be discarded;
- failed-flush semantics that preserve `Writeback` state and allow durability-only retry without rewriting blocks;
- focused regression coverage for cache misses, dirty writes, writeback, flush success/failure, eviction safety, and bounds.

The version-1 layout is documented in [`docs/on-disk-format.md`](docs/on-disk-format.md). Allocation, inode, directory, and cache state are intentionally in-memory only in this milestone; durable allocator, inode, namespace, or journal metadata requires an explicit future format revision rather than silently reinterpreting version 1.

The intended core progression is:

1. block layer;
2. versioned superblock/on-disk format;
3. allocation invariants;
4. inode lifecycle and directory model;
5. cache/dirty-state semantics;
6. journal/WAL and deterministic crash injection;
7. recovery and fsck invariants.

Large POSIX surface-area work, FUSE integration, complex extents, permissions, and other broad features are intentionally deferred until crash semantics and the durable core are well specified and executable.
