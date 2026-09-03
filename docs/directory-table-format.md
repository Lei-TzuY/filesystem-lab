# Directory table format v1

Filesystem format v5 reserves a contiguous directory-table region immediately after the inode table. The region stores a complete namespace snapshot as independently checksummed `DNT1` records.

## Region image

All integer fields are little-endian. The fixed region header is 32 bytes.

| Offset | Size | Field | v1 rule |
| ---: | ---: | --- | --- |
| 0 | 8 | magic | `FSLDIR\0\0` |
| 8 | 4 | version | `1` |
| 12 | 4 | flags | must be zero |
| 16 | 8 | payload bytes | exact concatenated-record byte count |
| 24 | 4 | record count | exact number of `DNT1` records |
| 28 | 4 | CRC-32 | IEEE CRC-32 over header + payload with this field treated as zero |
| 32 | variable | payload | concatenated `DNT1` records |
| after payload | variable | padding | must be zero through the end of the reserved region |

The image decoder validates the region checksum before accepting records, decodes exactly the declared payload, verifies the record count, and rejects non-zero trailing padding. Within one table, `(parent inode, name)` is unique; two entries with the same parent/name are corruption even when their targets differ.

## Rewrite ordering

Direct snapshot persistence writes non-header blocks first and writes the header-bearing first block last, then crosses the block-device `flush` boundary. A mixed old/new multi-block image is therefore expected to fail its checksum or padding invariants rather than be silently accepted.

`store_directory_table_journaled()` provides crash-atomic replacement within the bounded WAL. It renders the complete desired image, compares it with the durable directory region, and journals every changed directory-table block in one transaction. The journal image is flushed before recovery writes committed home blocks and crosses the home-location flush boundary. An identical snapshot is a no-op. If every changed block cannot fit in the fixed journal reservation, the update is rejected instead of being split across commits.

A crash before the commit record leaves the prior directory snapshot authoritative. After a durable commit, a crash or I/O failure during home writes is recovered idempotently from the still-durable journal.

## Relationship to filesystem format v5

The superblock records `directory_start` and `directory_blocks`. The directory region immediately follows the inode-table region and is part of `Superblock::reserved_blocks()`; allocator ownership bits for these metadata blocks remain clear. Fresh formatting initializes an empty valid directory-table image before publishing the v5 superblock.

The record payload format remains independently versioned by [`directory-entry-format.md`](directory-entry-format.md). A future filesystem format can relocate or resize the directory region without redefining `DNT1` records.

## Scope boundary

Journaled directory-table replacement is atomic only for the directory snapshot itself. This region format does not yet establish one transaction spanning directory + inode lifecycle changes, rename/unlink ordering, parent/target existence checks, reachability, hard-link counts, or root-directory policy. Those cross-layer invariants remain later bounded milestones.
