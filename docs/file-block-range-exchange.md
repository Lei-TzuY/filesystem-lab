# Regular-file logical-block range exchange

Format v5 supports a bounded crash-consistent primitive for exchanging equal-length contiguous logical-block ranges between two distinct existing regular files.

`exchange_file_block_ranges_journaled` swaps the exact physical-block references in place. It does not allocate, free, copy, or rewrite data blocks, so allocator ownership/accounting and both files' block counts remain unchanged. Namespace state and inode identities also remain unchanged.

Before WAL publication the operation validates distinct regular-file endpoints, both complete ranges, duplicate-reference safety, and allocator ownership of every referenced block. Only the changed inode-table image is published through one WAL transaction and the existing recovery/checkpoint path.

Deterministic crash testing must require old-or-complete-new inode mappings, clean post-recovery fsck, unchanged ownership/accounting and namespace, journal clearing, and second-recovery idempotence.

This does not add persisted byte length, EOF semantics, sparse holes, extents, reflinks, byte-range exchange, or POSIX compatibility, and it does not change the on-disk format version.
