# Pathname logical-block range transfer

`transfer_file_block_range_at_path_journaled` composes bounded absolute pathname resolution with the existing format-v5 `transfer_file_block_range_journaled` transaction.

Both source and destination follow intermediate and final symbolic links under the repository-wide bounded expansion rules. After resolution, one non-empty contiguous source block-reference range is removed from the source regular-file inode and inserted at a destination logical boundary. Physical data blocks are not copied or rewritten and allocator ownership is unchanged. The inode-table change is published through one WAL transaction, so recovery cannot leave a transferred block referenced by both files or by neither file.

The operation rejects identical resolved inodes, non-file endpoints, empty or overflowing source ranges, source ranges beyond file end, destination boundaries beyond the destination block vector, and allocator/reference disagreement before WAL publication.

Format v5 has no persisted byte length. This API is block-granular and intentionally does not claim byte-range move, EOF, sparse-hole, extent, reflink, or POSIX semantics.

Deterministic crash coverage enumerates every write/flush interruption and requires recovery to produce either the complete old inode table or the complete transferred inode table. Allocation and namespace images remain unchanged, fsck must accept every recovered image, the journal must checkpoint empty, and a second recovery must be a no-op.
