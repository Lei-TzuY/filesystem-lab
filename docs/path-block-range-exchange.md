# Pathname regular-file block-range exchange

Format v5 exposes equal-length, differently sized cross-file, and differently sized same-file crash-consistent logical-block range exchange through absolute pathname endpoints.

`exchange_file_block_ranges_at_path_journaled` resolves both paths with the repository-wide bounded symbolic-link rules, including final symlinks, and delegates the resolved inode IDs to `exchange_file_block_ranges_journaled`.

`exchange_variable_file_block_ranges_at_path_journaled` applies the same pathname semantics while preserving the independent block counts of both endpoints, then delegates to `exchange_variable_file_block_ranges_journaled`. The selected physical references trade ownership between the two inode block vectors, so either file may gain or lose logical blocks.

`exchange_same_file_block_ranges_at_path_journaled` resolves both pathname operands with the same bounded symbolic-link rules and requires them to resolve to one regular-file inode. It delegates two non-empty, disjoint ranges to `exchange_same_file_block_ranges_journaled`; ranges may have different lengths and are interpreted against the original block vector. Physical references are reordered within that one inode without allocation, freeing, or data copying.

All three operations are block-granular. They do not allocate, free, copy, rewrite, extend, or infer byte EOF. Physical block ownership and allocator accounting remain unchanged; only inode block-reference sequences are atomically exchanged through the existing WAL/checkpoint path. Cross-file operations require distinct regular-file endpoints, while the same-file operation requires both paths to resolve to the same inode and rejects overlapping ranges.

Deterministic crash tests enumerate every injected write/flush failure for pathname exchange, including variable-length cross-file and same-file cases. After recovery the inode table must equal either the complete pre-transaction image or the complete post-transaction image; allocator and namespace images remain unchanged, fsck reports a clean filesystem, the journal is cleared, and a second recovery is idempotent. These checks cover duplicate ownership, allocation accounting, and inode/block-reference consistency through the normal fsck invariants.
