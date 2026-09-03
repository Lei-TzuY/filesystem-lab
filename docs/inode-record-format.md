# Inode record format v1

The inode record codec is independently versioned from the filesystem superblock format. It is a prerequisite for a future durable inode-table region; this milestone does **not** yet assign inode records to filesystem blocks or change format v3.

Each record is self-delimiting and little-endian. The fixed 32-byte header is followed by `block_count` 64-bit block numbers.

| Offset | Size | Field |
| --- | ---: | --- |
| 0 | 4 | magic `INO1` |
| 4 | 2 | codec version (`1`) |
| 6 | 2 | kind (`1` file, `2` directory) |
| 8 | 4 | total record length |
| 12 | 8 | inode identifier |
| 20 | 4 | block reference count |
| 24 | 4 | IEEE CRC-32 |
| 28 | 4 | reserved, must be zero |
| 32 | `8 * block_count` | ordered block references |

The CRC is computed over the complete record with the CRC field treated as four zero bytes. Readers reject bad magic or version, unknown kinds, inode id zero, non-zero reserved bytes, inconsistent lengths, duplicate block references, checksum mismatch, and torn headers or payloads.

The codec deliberately preserves block-reference order because it is part of inode logical state. Duplicate references within one inode are invalid at the codec boundary. Cross-inode duplicate ownership, allocator agreement, reserved-region references, inode-table placement, journaling, and fsck integration remain responsibilities of later milestones.
