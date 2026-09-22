# Inode record format v3

The inode record codec is independently versioned from the filesystem superblock. Filesystem format
v6 writes inode-record version 3. Older inode-record versions are intentionally rejected by the v6
reader; there is no implicit reinterpretation or migration path.

Each record is self-delimiting and little-endian. The fixed 40-byte header is followed by
`block_count` 64-bit physical block numbers.

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 4 | magic `INO1` |
| 4 | 2 | codec version (`3`) |
| 6 | 2 | kind (`1` file, `2` directory, `3` symbolic link) |
| 8 | 4 | total record length |
| 12 | 8 | inode identifier |
| 20 | 4 | block reference count |
| 24 | 8 | exact regular-file byte EOF |
| 32 | 4 | IEEE CRC-32 |
| 36 | 4 | reserved, must be zero |
| 40 | `8 * block_count` | ordered block references |

The CRC covers the complete record with the CRC field treated as four zero bytes. Readers reject bad
magic/version, unknown kinds, inode id zero, non-zero reserved bytes, inconsistent lengths,
duplicate block references, impossible EOF/block-map combinations, checksum mismatch, and torn
headers or payloads.

For a regular file, zero blocks require byte length zero. A non-empty regular file requires
`0 < byte_len <= block_count * 4096`, and EOF must lie inside the final referenced block rather than
before it. This makes the block vector the complete non-sparse prefix of the file. Directory and
symlink inode records require byte length zero; symlink target length remains encoded in the symlink
payload itself.

Production inode creation goes through `PersistedInode::new` or
`PersistedInode::new_file_with_size`. The former retains block-granular compatibility by assigning
a regular file its full logical-block capacity as EOF; the latter accepts an exact EOF. Encoder-side
validation remains defensive, so direct compatibility struct literals cannot persist an impossible
record. The in-memory compatibility shorthand `byte_len = 0` on a non-empty regular file is
canonicalized to full block capacity before encoding and is never accepted as such from a decoded
v3 image.

Operations that change a persisted inode's logical block count use
`PersistedInode::replace_block_range`. The helper validates the complete candidate mapping before
commit and preserves the unused tail offset in the final block, so whole-block append/insert/remove
operations shift EOF coherently. `set_file_byte_len` is the explicit boundary for exact regular-file
EOF changes.

Format-v6 exact-byte shrink journals allocator release, inode EOF/block-map updates, and zeroing of
bytes discarded after a partial final EOF in one bounded transaction. Growth, sparse holes, and
general extent semantics remain outside this record milestone.
