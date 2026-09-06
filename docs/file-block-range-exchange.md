# Regular-file logical-block range exchange

Format v5 supports bounded crash-consistent primitives for exchanging contiguous logical-block ranges between two distinct existing regular files.

`exchange_file_block_ranges_journaled` swaps equal-length ranges in place. It does not allocate, free, copy, or rewrite data blocks, so allocator ownership/accounting and both files' block counts remain unchanged.

`exchange_variable_file_block_ranges_journaled` accepts two non-empty ranges with independently chosen block counts. The selected physical references trade positions between the two inode block vectors, so either file may gain or lose logical blocks while the union of physical references, allocator ownership/accounting, namespace state, inode identities, and data images remain unchanged.

Before WAL publication both operations validate distinct regular-file endpoints, complete in-bounds ranges, duplicate-reference safety, and allocator ownership of every referenced block. The variable-length operation also renders the complete resulting inode table before journal publication, so inode-table encoding or capacity failures occur before durable intent is recorded. Only changed inode-table home images are published through one WAL transaction and the existing recovery/checkpoint path.

Deterministic crash testing for the variable-length exchange requires old-or-complete-new inode mappings, clean post-recovery fsck, unchanged ownership/accounting and namespace, journal clearing, and second-recovery idempotence.

Neither primitive adds persisted byte length, EOF semantics, sparse holes, extents, reflinks, byte-range exchange, or broader POSIX compatibility, and neither changes the on-disk format version.
