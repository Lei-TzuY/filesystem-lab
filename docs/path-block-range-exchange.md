# Pathname regular-file block-range exchange

Format v5 exposes both equal-length and differently sized crash-consistent logical-block range exchange through absolute pathname endpoints.

`exchange_file_block_ranges_at_path_journaled` resolves both paths with the repository-wide bounded symbolic-link rules, including final symlinks, and delegates the resolved inode IDs to `exchange_file_block_ranges_journaled`.

`exchange_variable_file_block_ranges_at_path_journaled` applies the same pathname semantics while preserving the independent block counts of both endpoints, then delegates to `exchange_variable_file_block_ranges_journaled`. The selected physical references trade ownership between the two inode block vectors, so either file may gain or lose logical blocks.

Both operations are block-granular. They require distinct regular-file endpoints, non-empty ranges, and intervals that already fit inside both files. They do not allocate, free, copy, rewrite, extend, or infer byte EOF. Physical block ownership and allocator accounting remain unchanged; only inode block-reference sequences are atomically exchanged through the existing WAL/checkpoint path.

Deterministic crash tests enumerate every injected write/flush failure for pathname exchange, including a variable-length case that changes both inode block counts. After recovery the inode table must equal either the complete pre-transaction image or the complete post-transaction image; allocator and namespace images remain unchanged, fsck reports a clean filesystem, the journal is cleared, and a second recovery is idempotent. These checks cover duplicate ownership, allocation accounting, and inode/block-reference consistency through the normal fsck invariants.
