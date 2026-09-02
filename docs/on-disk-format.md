# On-disk format

## Version 1

Version 1 used filesystem block 0 as a superblock containing only magic, format version, logical block size, and total block count. Bytes 24..4096 were reserved and required to be zero. It defined no durable journal reservation.

Version 1 images are intentionally not accepted by newer readers. The laboratory currently has no migration path; changing durable semantics requires an explicit format version rather than silently reinterpreting old bytes.

## Version 2

Version 2 added an explicit contiguous journal reservation immediately after the superblock. Its superblock fields ended at byte 40 and all later bytes were reserved and required to be zero. Allocation state remained in-memory only.

Version 2 images are intentionally rejected by the version-3 reader. The schema is retained here as historical documentation rather than reinterpreted in place.

## Version 3

Filesystem block 0 remains the superblock. All integer fields are little-endian. Version 3 adds an explicit allocation-metadata reservation immediately after the journal. The rest of the 4 KiB superblock is reserved and MUST be zero.

| Offset | Size | Field | Version 3 value |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | `FSLABFS\0` |
| 8 | 4 | format version | `3` |
| 12 | 4 | logical block size | `4096` |
| 16 | 8 | total block count | exact backing-device block count |
| 24 | 8 | journal start block | `1` |
| 32 | 8 | journal block count | non-zero and fully inside the device |
| 40 | 8 | allocation start block | exactly `journal_start + journal_block_count` |
| 48 | 8 | allocation block count | exact size required by allocation image v1 |
| 56 | 4040 | reserved | all zero |

The journal occupies `journal_start..journal_start + journal_block_count`. The allocation image occupies `allocation_start..allocation_start + allocation_block_count`. The complete durable metadata prefix is therefore `0..Superblock::reserved_blocks()`.

Allocation-image capacity is deterministic from `total_blocks`: one bit is reserved for every filesystem block, plus a 32-byte allocation-image header, rounded up to 4 KiB blocks. The bitmap represents **data-block ownership only**; bits corresponding to the durable metadata prefix MUST remain zero. Bits beyond `total_blocks` in the final bitmap byte MUST also remain zero. This keeps reserved metadata ownership implicit in the superblock geometry and prevents the bitmap from claiming the blocks that contain itself.

A version-3 implementation MUST reject a superblock when the magic, version, block size, journal geometry, allocation geometry, reserved bytes, or total block count is invalid. Journal and allocation range arithmetic must be checked for overflow, and both reservations must remain fully inside the filesystem. Opening also validates that the recorded total block count exactly matches the currently opened block device.

Formatting initializes the allocation image before publishing block zero, then writes the superblock and crosses the block-device durability boundary with `flush`. The default formatter reserves two journal blocks. `Superblock::with_journal_blocks` remains available so tests and later tooling can construct deterministic alternative journal sizes while the allocation reservation is still derived from total filesystem size.

Version 3 defines the location and ownership of the journal and allocation regions. The journal bytes use the independently versioned bounded journal-region image documented in [`journal-region-format.md`](journal-region-format.md), whose payload is the independently versioned record stream documented in [`journal-record-format.md`](journal-record-format.md). Allocation bytes use allocation image v1: a checksummed header plus bitmap payload with strict zero padding. Multi-block allocation updates write tail blocks first and the header-bearing first block last, so a torn mixed image is rejected by checksum validation instead of silently accepted.

Inode and directory persistence, allocator updates through the journal, checkpointing, and circular journal head/tail metadata remain undefined. Direct allocation-image persistence is intentionally a bounded laboratory primitive rather than the final crash-atomic allocator update path; future metadata transactions should route allocation changes through the journal before broader filesystem semantics are added.
