# Journal record format

This document defines the standalone persistent journal-record codec. It is intentionally versioned independently from the filesystem superblock format.

The current filesystem format version 2 reserves a durable journal region but does **not** yet define circular-log placement, head/tail metadata, checkpointing, or recovery writes. Consequently, record codec version 1 does not by itself make the journal region recoverable; it defines only how an individual ordered stream of logical `Begin`, `Write`, and `Commit` records is represented and validated once such a stream is supplied.

## Codec version 1

Every record begins with a 32-byte little-endian header.

| Offset | Size | Field | Requirement |
| ---: | ---: | --- | --- |
| 0 | 4 | magic | ASCII `JNL1` |
| 4 | 2 | codec version | `1` |
| 6 | 1 | record kind | `1` begin, `2` write, `3` commit |
| 7 | 1 | flags | `0` |
| 8 | 4 | total record length | header + payload, at least 32 |
| 12 | 8 | transaction id | little-endian `u64` |
| 20 | 8 | target block | write target; zero for begin/commit |
| 28 | 4 | CRC-32 | IEEE CRC-32 over the complete record with this field treated as zero |

Begin and commit records have no payload and therefore have total length 32. Their target-block field MUST be zero.

Write records contain exactly one logical filesystem block (4096 bytes) immediately after the header, so their total record length is 4128 bytes. The write target is stored in the header's target-block field.

A decoder MUST reject truncated headers, truncated payloads, invalid magic, unsupported codec versions or flags, unknown record kinds, impossible lengths, malformed control records, write records whose payload is not exactly one logical block, and checksum mismatches. A torn record therefore cannot be accepted as a complete logical journal entry.

The codec validates record integrity only. Transaction-order invariants such as nested transactions, mismatched transaction ids, and commit ordering remain the responsibility of `JournalImage::validate` / replay semantics. A later milestone will bind codec records to the filesystem's reserved journal region and define durable log position and recovery behavior.
