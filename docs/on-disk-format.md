# On-disk format

## Version 1

Filesystem block 0 is the superblock. All integer fields are little-endian. The rest of the 4 KiB superblock is reserved and MUST be zero so future format revisions cannot be silently mistaken for version 1.

| Offset | Size | Field | Version 1 value |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | `FSLABFS\0` |
| 8 | 4 | format version | `1` |
| 12 | 4 | logical block size | `4096` |
| 16 | 8 | total block count | exact backing-device block count, non-zero |
| 24 | 4072 | reserved | all zero |

A filesystem implementation MUST reject a superblock when the magic, version, block size, reserved bytes, or total block count is invalid. Opening also validates that the recorded total block count exactly matches the currently opened block device.

Formatting writes block 0 and then calls the block device durability boundary (`flush`). Version 1 deliberately defines no allocator, inode, directory, checksum, journal, or recovery metadata yet; those remain later milestones and must extend the format through explicit versioning rather than reinterpreting existing fields.
