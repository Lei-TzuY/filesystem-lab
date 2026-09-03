# On-disk format

## Version 1

Version 1 used filesystem block 0 as a superblock containing only magic, format version, logical block size, and total block count. Bytes 24..4096 were reserved and required to be zero. It defined no durable journal reservation.

Version 1 images are intentionally not accepted by newer readers. The laboratory currently has no migration path; changing durable semantics requires an explicit format version rather than silently reinterpreting old bytes.

## Version 2

Version 2 added an explicit contiguous journal reservation immediately after the superblock. Its superblock fields ended at byte 40 and all later bytes were reserved and required to be zero. Allocation state remained in-memory only.

Version 2 images are intentionally rejected by newer readers.

## Version 3

Version 3 added a durable allocation-metadata reservation immediately after the journal. Its superblock fields ended at byte 56. Allocation bytes use allocation image v1: a checksummed bitmap image whose reserved/trailing bits and padding are required to remain zero.

Version 3 images are intentionally rejected by newer readers.

## Version 4

Version 4 added an explicit inode-table reservation immediately after allocation metadata. Its superblock fields ended at byte 72. The inode-table payload uses independently versioned `INO1` records inside a checksummed inode-table region image.

Version 4 images are intentionally rejected by the version-5 reader; there is no implicit upgrade path.

## Version 5

Filesystem block 0 remains the superblock. All integer fields are little-endian. Version 5 adds an explicit directory-table reservation immediately after the inode table. The rest of the 4 KiB superblock is reserved and MUST be zero.

| Offset | Size | Field | Version 5 value |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | `FSLABFS\0` |
| 8 | 4 | format version | `5` |
| 12 | 4 | logical block size | `4096` |
| 16 | 8 | total block count | exact backing-device block count |
| 24 | 8 | journal start block | `1` |
| 32 | 8 | journal block count | non-zero and fully inside the device |
| 40 | 8 | allocation start block | exactly `journal_start + journal_block_count` |
| 48 | 8 | allocation block count | exact size required by allocation image v1 |
| 56 | 8 | inode-table start block | exactly `allocation_start + allocation_block_count` |
| 64 | 8 | inode-table block count | non-zero and fully inside the device |
| 72 | 8 | directory-table start block | exactly `inode_start + inode_block_count` |
| 80 | 8 | directory-table block count | non-zero and fully inside the device |
| 88 | 4008 | reserved | all zero |

The durable metadata prefix is ordered as superblock → journal → allocation image → inode table → directory table. `Superblock::reserved_blocks()` returns the first ordinary data block after the directory table.

Allocation-image capacity remains deterministic from `total_blocks`: one bit per filesystem block plus the 32-byte allocation-image header, rounded up to 4 KiB blocks. The bitmap represents ordinary data-block ownership only; every bit corresponding to the complete version-5 metadata prefix MUST remain zero.

The inode table uses [`inode-table-format.md`](inode-table-format.md), with records defined by [`inode-record-format.md`](inode-record-format.md). The directory table uses [`directory-table-format.md`](directory-table-format.md), with records defined by [`directory-entry-format.md`](directory-entry-format.md). The default formatter reserves two blocks each for inode and directory tables. `Superblock::with_metadata_blocks` retains the default directory reservation for existing callers; `Superblock::with_all_metadata_blocks` exposes all three explicit reservation sizes.

A version-5 implementation MUST reject invalid magic, version, block size, journal geometry, allocation geometry, inode geometry, directory geometry, reserved bytes, arithmetic overflow, or a recorded total block count that differs from the opened block device.

Formatting initializes allocation, empty inode-table, and empty directory-table images before publishing block zero, then writes the superblock and crosses the block-device durability boundary with `flush`. A successfully published version-5 superblock therefore never points at an uninitialized durable metadata reservation.

The journal region remains independently versioned as documented in [`journal-region-format.md`](journal-region-format.md), with records documented in [`journal-record-format.md`](journal-record-format.md). Allocation, inode-table, and directory-table snapshots can all be persisted through the bounded WAL; cross-table inode+directory and allocation+inode+directory transactions are implemented for lifecycle consistency. Validated create, unlink, and bounded rename paths build on those primitives, with deterministic crash matrices and read-only fsck checking their post-recovery invariants. Checkpointing, journal clearing/circular head-tail metadata, file-data persistence semantics, and broader POSIX lifecycle behavior remain outside the current v5 durable metadata-core checkpoint.
