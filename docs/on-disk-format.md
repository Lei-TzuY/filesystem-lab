# On-disk format

## Version 1

Version 1 used filesystem block 0 as a superblock containing only magic, format version, logical block size, and total block count. Bytes 24..4096 were reserved and required to be zero. It defined no durable journal reservation.

Version 1 images are intentionally not accepted by a version-2 reader. The laboratory currently has no migration path; changing durable semantics requires an explicit format version rather than silently reinterpreting old bytes.

## Version 2

Filesystem block 0 remains the superblock. All integer fields are little-endian. Version 2 adds an explicit contiguous journal reservation immediately after the superblock. The rest of the 4 KiB superblock is reserved and MUST be zero.

| Offset | Size | Field | Version 2 value |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | `FSLABFS\0` |
| 8 | 4 | format version | `2` |
| 12 | 4 | logical block size | `4096` |
| 16 | 8 | total block count | exact backing-device block count |
| 24 | 8 | journal start block | `1` |
| 32 | 8 | journal block count | non-zero and fully inside the device |
| 40 | 4056 | reserved | all zero |

The journal occupies the half-open range `journal_start..journal_start + journal_block_count`. Because `journal_start` is fixed at block 1, the complete currently defined metadata prefix is `0..1 + journal_block_count`. The allocator can therefore exclude the superblock and journal region using `Superblock::reserved_blocks()` without duplicating layout constants.

A version-2 implementation MUST reject a superblock when the magic, version, block size, journal start, journal length, reserved bytes, or total block count is invalid. Journal-range arithmetic must be checked for overflow, the journal must contain at least one block, and the journal end must not exceed the filesystem size. Opening also validates that the recorded total block count exactly matches the currently opened block device.

Formatting writes block 0 and then calls the block device durability boundary (`flush`). The default formatter currently reserves one journal block. `Superblock::with_journal_blocks` exists so tests and later format tooling can construct larger reservations deterministically.

Version 2 defines only the **location and ownership** of the journal region. It does not yet define persistent journal-record encoding, circular-log head/tail metadata, checksums, checkpointing, or recovery writes. The in-memory `JournalLog` remains the executable source of transaction ordering and replay semantics until those later milestones map it onto this reserved region.

Allocator bitmap, inode, and directory metadata also remain non-durable and require future explicitly versioned extensions if their representation changes the superblock or reserved-region contract.
