# Journal region image format

The filesystem version-2 superblock reserves a contiguous journal region. This document defines the first bounded persistent image stored inside that reservation. The region-image format is versioned independently from both the filesystem superblock and the journal-record codec.

## Version 1

The first 32 bytes of the reserved journal region form a little-endian header:

| Offset | Size | Field | Version 1 value |
| ---: | ---: | --- | --- |
| 0 | 4 | magic | `JRG1` |
| 4 | 2 | region version | `1` |
| 6 | 2 | flags | `0` |
| 8 | 8 | encoded payload length | number of journal-record bytes after the header |
| 16 | 4 | CRC-32 | IEEE CRC-32 over header + payload with this field treated as zero |
| 20 | 12 | reserved | all zero |
| 32 | variable | payload | journal-record codec stream |
| after payload | remainder | padding | all zero |

A completely zeroed reservation is the canonical freshly formatted / never-written journal state and decodes as an empty journal.

The payload must fit completely inside the superblock-declared journal reservation. Non-zero trailing padding is invalid; this makes stale or partially overwritten tails observable rather than silently ignored. The decoder also validates the journal-record codec and transaction ordering after the region checksum succeeds.

Journal writes may target only filesystem blocks at or above `Superblock::reserved_blocks()` and below `total_blocks`. A persistent journal image is therefore not allowed to overwrite the superblock or its own journal reservation.

## Write ordering

A bounded image is materialized in memory with zero-filled padding. When more than one journal block is reserved, blocks after the first journal block are issued from the tail toward the front. The first journal block, which contains the region header and the initial payload bytes, is written last. Only after all region blocks have been issued does the implementation call the block-device `flush` durability boundary.

This ordering deliberately makes the header-bearing block the final anchor for a new image. Recovery does not assume that a crash produced a valid image: an old anchor combined with new tail blocks, a new anchor combined with stale/torn payload, checksum corruption, non-zero stale padding, or malformed records are all rejected deterministically.

This is a bounded image, not yet a circular journal. Version 1 defines no head/tail wraparound, checkpoint sequence, generation counter, or home-location replay writes. Those remain later milestones.
