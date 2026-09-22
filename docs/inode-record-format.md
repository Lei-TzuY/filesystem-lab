# Inode record format v2

The inode record codec is independently versioned from the filesystem superblock format. Filesystem format v5 now writes inode-record version 2. Version 2 preserves the existing record geometry and adds an explicit symbolic-link inode kind; readers intentionally reject older inode-record versions rather than silently reinterpreting them.

Each record is self-delimiting and little-endian. The fixed 32-byte header is followed by `block_count` 64-bit block numbers.

| Offset | Size | Field |
| --- | ---: | --- |
| 0 | 4 | magic `INO1` |
| 4 | 2 | codec version (`2`) |
| 6 | 2 | kind (`1` file, `2` directory, `3` symbolic link) |
| 8 | 4 | total record length |
| 12 | 8 | inode identifier |
| 20 | 4 | block reference count |
| 24 | 4 | IEEE CRC-32 |
| 28 | 4 | reserved, must be zero |
| 32 | `8 * block_count` | ordered block references |

The CRC is computed over the complete record with the CRC field treated as four zero bytes. Readers reject bad magic or version, unknown kinds, inode id zero, non-zero reserved bytes, inconsistent lengths, duplicate block references, checksum mismatch, and torn headers or payloads.

The codec deliberately preserves block-reference order because it is part of inode logical state. Duplicate references within one inode are invalid at the codec boundary. Cross-inode duplicate ownership and allocator agreement remain fsck responsibilities.

Production creation of durable inode values goes through `PersistedInode::new`, which enforces identifier and duplicate-block invariants before a value reaches inode-table or WAL publication. The encoder remains defensive and re-validates those invariants so legacy callers that still construct the public compatibility struct directly cannot bypass durable validation. This construction boundary is intentionally separate from the v2 field layout so a future record revision can centralize new persisted-field initialization rather than duplicating it across pathname create and symlink code.

Symbolic-link payload semantics are defined separately in [`symlinks.md`](symlinks.md). The bounded symbolic-link slice uses exactly one owned data block per symlink inode and does not add path traversal, target resolution, permissions, or broad POSIX semantics.
