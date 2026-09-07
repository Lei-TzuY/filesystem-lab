# Pathname regular-file block-range exchange

Format v5 exposes the existing crash-consistent equal-length logical-block range exchange through absolute pathname endpoints.

`exchange_file_block_ranges_at_path_journaled` resolves both paths with the repository-wide bounded symbolic-link rules, including final symlinks, and delegates the resolved inode IDs to `exchange_file_block_ranges_journaled`.

The operation is block-granular. It requires distinct regular-file endpoints, a non-zero block count, and ranges that already fit inside both files. It does not allocate, free, copy, rewrite, extend, or infer byte EOF. Physical block ownership and allocator accounting are unchanged; only the two inode block-reference sequences are atomically exchanged through the existing WAL/checkpoint path.

Deterministic crash tests enumerate every injected write/flush failure for a pathname exchange. After recovery the inode table must equal either the complete pre-transaction image or the complete post-transaction image; allocator and namespace images remain unchanged, fsck reports a clean filesystem, the journal is cleared, and a second recovery is idempotent. These checks cover duplicate ownership and inode/block-reference consistency through the normal fsck invariants.
