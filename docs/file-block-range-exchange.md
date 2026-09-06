# Regular-file logical-block range exchange

Format v5 supports bounded crash-consistent primitives for exchanging contiguous logical-block ranges between regular files and, for disjoint ranges, within one regular file.

`exchange_file_block_ranges_journaled` swaps equal-length ranges between two distinct files in place. It does not allocate, free, copy, or rewrite data blocks, so allocator ownership/accounting and both files' block counts remain unchanged.

`exchange_variable_file_block_ranges_journaled` accepts two non-empty ranges in distinct files with independently chosen block counts. The selected physical references trade positions between the two inode block vectors, so either file may gain or lose logical blocks while the union of physical references, allocator ownership/accounting, namespace state, inode identities, and data images remain unchanged.

`exchange_same_file_block_ranges_journaled` accepts two non-empty, non-overlapping ranges in one regular file, with coordinates interpreted against the original block vector. The ranges may have different lengths. If the earlier range is `A`, the intervening logical blocks are `M`, and the later range is `B`, the resulting sequence is `prefix + B + M + A + suffix`. No physical block changes ownership and the file's total block count is unchanged.

Before WAL publication the cross-file operations validate regular-file endpoints, complete in-bounds ranges, duplicate-reference safety, and allocator ownership of every referenced block. The same-file operation additionally requires both range descriptors to target one inode and rejects overlap. Each operation renders the complete resulting inode table before journal publication, so inode-table encoding or capacity failures occur before durable intent is recorded. Only changed inode-table home images are published through one WAL transaction and the existing recovery/checkpoint path.

Deterministic crash testing requires old-or-complete-new inode mappings, clean post-recovery fsck, unchanged ownership/accounting and namespace, journal clearing, and second-recovery idempotence. Same-file exchange crash enumeration also verifies that no crash prefix can expose a partially reordered inode block vector.

These primitives do not add persisted byte length, EOF semantics, sparse holes, extents, reflinks, byte-range exchange, or broader POSIX compatibility, and they do not change the on-disk format version.
