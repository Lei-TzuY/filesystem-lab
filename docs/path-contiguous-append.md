# Contiguous pathname multi-block append

`append_file_blocks_contiguous_at_path_journaled()` appends one or more complete 4 KiB logical blocks to an existing regular file while requiring the newly allocated physical blocks to occupy the allocator's lowest-numbered contiguous free run large enough for the whole append.

Before pathname resolution, the wrapper recovers and checkpoints any older committed WAL. The final target follows the existing bounded symbolic-link resolver. Allocation ownership, inode block-vector growth, and all appended data images are then published through one WAL transaction.

`append_zeroed_blocks_contiguous_at_path_journaled()` is the block-granular zero-extension form of the same contract. A non-zero block count extends the resolved regular file with independently owned, physically contiguous blocks whose durable data images are all zero. This is real file growth rather than sparse or unwritten preallocation: the inode's explicit logical block vector grows by the requested count and each new block participates in normal ownership accounting.

Both operations are all-or-nothing with respect to the modeled write/flush crash points: recovery exposes either the old inode/allocator/data state or the complete appended run. Crash tests for zero extension additionally require every newly visible logical block to read as zero. The matrices require unique ownership, allocation accounting, unchanged namespace, clean fsck, an empty checkpointed journal, and idempotent second recovery.

This does **not** change filesystem format v5. The inode still persists an explicit block vector, not extent records. Contiguity is only an allocation policy guarantee for this append; the new run need not be adjacent to the file's previous final block, and later mutations may fragment the file. There is no sparse-file, unwritten-extent, reservation, or byte-level EOF semantic change.
