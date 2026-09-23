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

The journal remains bounded rather than circular. Version 3 can retain multiple complete
transactions inside one alternating-bank snapshot, while persistent head/tail wraparound and
retention beyond the bounded bank capacity remain outside this milestone.


## Version 3 retained snapshots

Version 3 adds an optional bounded retained-log publication mode without changing filesystem
superblock geometry. The first journal block is a checksummed anchor and the remaining reservation is
split into two equal-sized banks. The anchor records the active bank, a monotonically increasing
generation, the encoded payload length, the payload CRC-32, and its own header CRC-32.

A retained publication writes the complete replacement journal stream to the inactive bank, flushes
that bank, then atomically replaces the first-block anchor and flushes again. The old active bank is
never overwritten before the new bank is durable. Under the repository's whole-block crash model,
reboot therefore observes either the previous complete retained snapshot or the new complete
snapshot.

The first retained snapshot may be created from the canonical v2 empty anchor or a zeroed legacy
empty reservation. Once v3 is active, later retained appends alternate banks and carry forward the
already-retained complete transactions. Active v1/v2 logs must be recovered and checkpointed before
entering retained mode.

Only complete committed transactions may be appended through the retained API. A retained snapshot
with an incomplete tail is rejected for further append, keeping the bank-switch boundary itself a
complete-transaction durability point.

Checkpointing remains compatible: after all retained committed transactions are replayed and home
writes are durable, the existing v2 empty anchor invalidates the v3 snapshot in one first-block
transition. Stale bank bytes are then non-authoritative.

This is still a bounded snapshot journal. Bank capacity is half of the reservation after the anchor
block, and there is no persistent circular head/tail, wraparound reuse, or arbitrary-duration
transaction retention.
