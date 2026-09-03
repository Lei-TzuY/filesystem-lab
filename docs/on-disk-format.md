# On-disk format

## Version 1

Version 1 used filesystem block 0 as a superblock containing only magic, format version, logical block size, and total block count. Bytes 24..4096 were reserved and required to be zero. It defined no durable journal reservation.

Version 1 images are intentionally not accepted by newer readers. The laboratory currently has no migration path; changing durable semantics requires an explicit format version rather than silently reinterpreting old bytes.

## Version 2

Version 2 added an explicit contiguous journal reservation immediately after the superblock. Its superblock fields ended at byte 40 and all later bytes were reserved and required to be zero. Allocation state remained in-memory only.

Version 2 images are intentionally rejected by newer readers. The schema is retained here as historical documentation rather than reinterpreted in place.

## Version 3

Version 3 added a durable allocation-metadata reservation immediately after the journal. Its superblock fields ended at byte 56. Allocation bytes used allocation image v1: a checksummed bitmap image whose reserved/trailing bits and padding are required to remain zero.

Version 3 images are intentionally rejected by the version-4 reader. There is no implicit upgrade path.

## Version 4

Filesystem block 0 remains the superblock. All integer fields are little-endian. Version 4 adds an explicit inode-table reservation immediately after allocation metadata. The rest of the 4 KiB superblock is reserved and MUST be zero.

| Offset | Size | Field | Version 4 value |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | `FSLABFS\0` |
| 8 | 4 | format version | `4` |
| 12 | 4 | logical block size | `4096` |
| 16 | 8 | total block count | exact backing-device block count |
| 24 | 8 | journal start block | `1` |
| 32 | 8 | journal block count | non-zero and fully inside the device |
| 40 | 8 | allocation start block | exactly `journal_start + journal_block_count` |
| 48 | 8 | allocation block count | exact size required by allocation image v1 |
| 56 | 8 | inode-table start block | exactly `allocation_start + allocation_block_count` |
| 64 | 8 | inode-table block count | non-zero and fully inside the device |
| 72 | 4024 | reserved | all zero |

The durable metadata prefix is ordered as superblock → journal → allocation image → inode table. `Superblock::reserved_blocks()` returns the first data block after the inode table.

Allocation-image capacity remains deterministic from `total_blocks`: one bit per filesystem block plus the 32-byte allocation-image header, rounded up to 4 KiB blocks. The bitmap represents data-block ownership only; every bit corresponding to the complete version-4 metadata prefix MUST remain zero.

The inode table uses the independently versioned region image documented in [`inode-table-format.md`](inode-table-format.md). Its payload consists of independently versioned `INO1` records documented in [`inode-record-format.md`](inode-record-format.md). The default formatter reserves two inode-table blocks. `Superblock::with_metadata_blocks` permits deterministic alternative journal/inode reservations for tests and later tooling.

A version-4 implementation MUST reject invalid magic, version, block size, journal geometry, allocation geometry, inode geometry, reserved bytes, arithmetic overflow, or a recorded total block count that differs from the opened block device.

Formatting initializes the allocation image and empty inode-table image before publishing block zero, then writes the superblock and crosses the block-device durability boundary with `flush`. Thus a successfully published version-4 superblock never points at an uninitialized allocation or inode reservation.

The journal bytes still use the independently versioned bounded journal-region image documented in [`journal-region-format.md`](journal-region-format.md), whose payload is the independently versioned record stream documented in [`journal-record-format.md`](journal-record-format.md). Allocation mutations may already be routed through the WAL. Inode-table persistence in this version is deliberately only a direct checksummed snapshot primitive; journaled inode mutation, cross-layer inode/allocation ownership fsck, directory persistence, checkpointing, and circular journal head/tail metadata remain future milestones.
