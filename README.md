# filesystem-lab

A focused filesystem implementation and crash-consistency laboratory for building and verifying a small filesystem stack from first principles, with explicit persistence and corruption invariants.

## Current milestone

The repository now establishes the block-device foundation, a versioned on-disk superblock, executable allocation invariants, in-memory inode/directory models, an explicit buffer-cache durability state machine, deterministic journal replay semantics, an explicit durable journal reservation, and a standalone persistent journal-record codec:

- fixed 4 KiB logical blocks;
- file-backed block device creation/opening;
- strict block-index bounds checking;
- checked device-size/offset arithmetic;
- rejection of non-block-aligned backing files;
- explicit durable flush boundary via `sync_data`;
- version-2 superblock at block zero with magic, format version, block size, total block count, journal start, and journal length;
- explicit contiguous journal reservation immediately after the superblock, with checked range arithmetic and bounds validation;
- a single `reserved_blocks()` boundary that lets allocation exclude all currently defined durable metadata;
- strict superblock validation, including journal-layout, reserved-byte, and device-size consistency checks;
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
- logical journal transactions with explicit begin, full-block write, and commit records;
- deterministic crash-prefix construction at every journal-entry boundary;
- replay semantics that ignore uncommitted tails and atomically apply a transaction only when its commit marker is present;
- malformed-log validation for nested transactions, mismatched transaction identifiers, writes outside transactions, and invalid commits;
- standalone journal record codec v1 with magic/version/kind/length/transaction/block fields and IEEE CRC-32 integrity;
- deterministic little-endian encoding for begin, full-block write, and commit records;
- rejection of torn headers/payloads, checksum corruption, unsupported versions/flags, unknown record kinds, and malformed record lengths;
- focused regression coverage for cache durability, journal crash-before/after-commit behavior, durable journal-region layout validation, and persistent record corruption.

The current filesystem version-2 layout is documented in [`docs/on-disk-format.md`](docs/on-disk-format.md). Version 1 is retained in the document as historical schema information and is intentionally rejected by the version-2 reader; the laboratory does not silently reinterpret old images. The standalone journal record codec is documented separately in [`docs/journal-record-format.md`](docs/journal-record-format.md) and is versioned independently. Allocation, inode, and directory contents remain in-memory only. Version 2 reserves durable ownership of the journal region, while circular-log placement, head/tail metadata, checkpointing, recovery writes, and binding the record stream to that region remain intentionally undefined.

The intended core progression is:

1. block layer;
2. versioned superblock/on-disk format;
3. allocation invariants;
4. inode lifecycle and directory model;
5. cache/dirty-state semantics;
6. journal transaction/replay semantics and deterministic crash prefixes;
7. durable journal region layout;
8. persistent journal record encoding;
9. journal-region binding and recovery;
10. fsck/corruption invariants.

Large POSIX surface-area work, FUSE integration, complex extents, permissions, and other broad features are intentionally deferred until crash semantics and the durable core are well specified and executable.
