# Journal region image format

The filesystem reserves one bounded journal region. The region codec is versioned independently from
the filesystem superblock and from the journal-record codec.

## Version 2

Writers publish version 2. The first 32 bytes of the first journal block form a little-endian anchor:

| Offset | Size | Field | Version 2 |
| ---: | ---: | --- | --- |
| 0 | 4 | magic | `JRG2` |
| 4 | 2 | region version | `2` |
| 6 | 2 | state | `0` = empty, `1` = active |
| 8 | 8 | encoded payload length | zero for empty; journal-record bytes for active |
| 16 | 4 | CRC-32 | IEEE CRC-32 with this field treated as zero |
| 20 | 12 | reserved | all zero |
| 32 | variable | payload | journal-record stream when active |

A v2 empty anchor has zero payload length. Its checksum covers the 32-byte anchor, and the remainder
of the first journal block is zero. Bytes in later journal blocks are explicitly non-authoritative
while the empty anchor is present; they may contain stale bytes from the previous active image.

An active v2 image retains strict padding and checksum rules. The complete active payload must fit the
reservation, all bytes after the used image are zero, the checksum covers anchor plus payload, and the
decoded record stream must satisfy journal transaction and target-block validation.

## Publication ordering

The `BlockDevice` contract guarantees that `flush` makes prior writes durable but does not guarantee
that writes were non-durable before that point. Publication therefore uses an anchor protocol:

1. publish and flush a v2 empty anchor;
2. write all journal blocks after the first from tail toward the front;
3. flush those staged tail blocks;
4. write the first block containing the active anchor and initial payload;
5. flush the active anchor.

If a crash occurs before step 4 becomes durable, the journal is authoritatively empty even if some
tail writes persisted early. If the active anchor is durable, every tail block it references has
already crossed a durability barrier.

Checkpoint is the inverse transition after home replay is durable: only the first block is replaced
with a checksummed empty anchor and flushed. Stale tail bytes are intentionally ignored until a later
publication stages a complete replacement image.

## Version 1 compatibility

Readers continue to accept complete `JRG1` / version-1 images. Version 1 uses the same length and
checksum offsets, requires flags at offset 6 to be zero, and requires zero trailing padding across the
whole reservation. This permits recovery/checkpoint of existing complete v1 images without silently
reinterpreting them. New writes use version 2.

A completely zeroed reservation remains accepted as a legacy/uninitialized empty state. Successful
current formatting writes the canonical v2 empty anchor instead.

Journal writes may target ordinary data blocks and allocation/inode/directory home regions. They may
never target the superblock or the journal reservation itself.

The journal is still bounded rather than circular; persistent head/tail wraparound and multi-
transaction retention remain outside this milestone.
