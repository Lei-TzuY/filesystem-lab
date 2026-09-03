# Inode table image format

Filesystem format v4 reserves a contiguous inode-table region immediately after the allocation image. The region stores a complete snapshot of durable inode records and is independently versioned from both the filesystem superblock and the `INO1` inode-record codec.

## Region image v1

All integers are little-endian. The first 32 bytes are the region header:

| Offset | Size | Field | Value |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | `FSLINOD\0` |
| 8 | 4 | version | `1` |
| 12 | 4 | flags | `0` |
| 16 | 8 | payload bytes | exact byte length of concatenated records |
| 24 | 4 | record count | number of `INO1` records |
| 28 | 4 | CRC-32 | IEEE CRC-32 over header plus payload with this field zeroed |

The payload begins at byte 32 and is a concatenation of the self-delimiting records documented in [`inode-record-format.md`](inode-record-format.md). Inode identifiers MUST be non-zero and unique across the image. The decoder consumes exactly `record_count` records and requires the consumed byte count to equal `payload bytes`.

Every byte after the payload through the end of the reserved inode region MUST be zero. This prevents a shorter rewrite from leaving stale records that could later be mistaken for live metadata.

## Write ordering and crash boundary

A complete inode-table snapshot is encoded and checksummed before any device write. For a multi-block region, tail blocks are issued first and the header-bearing first block is issued last; a successful store then calls the block-device `flush` operation. Consequently, a mixed old/new image caused by a failed write is not considered a valid committed table: checksum or strict-padding validation must reject it.

This milestone does **not** make inode mutation journal-atomic. Direct inode-table persistence is a bounded primitive used to establish durable placement and corruption detection. A later milestone must route inode-table changes through the WAL before inode lifecycle operations can claim crash-atomic persistence.
