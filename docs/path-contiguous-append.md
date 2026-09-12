# Contiguous pathname multi-block append

`append_file_blocks_contiguous_at_path_journaled()` appends one or more complete 4 KiB logical blocks to an existing regular file while requiring the newly allocated physical blocks to occupy the allocator's lowest-numbered contiguous free run large enough for the whole append.

Before pathname resolution, the wrapper recovers and checkpoints any older committed WAL. The final target follows the existing bounded symbolic-link resolver. Allocation ownership, inode block-vector growth, and all appended data images are then published through one WAL transaction.

The operation is all-or-nothing with respect to the modeled write/flush crash points: recovery exposes either the old inode/allocator/data state or the complete appended run. Crash tests also require unique ownership, allocation accounting, unchanged namespace, clean fsck, an empty checkpointed journal, and idempotent second recovery.

This does **not** change filesystem format v5. The inode still persists an explicit block vector, not extent records. Contiguity is only an allocation policy guarantee for this append; the new run need not be adjacent to the file's previous final block, and later mutations may fragment the file. There is no sparse-file or byte-level EOF semantic change.
