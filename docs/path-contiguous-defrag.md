# Contiguous pathname file defragmentation

`defragment_file_contiguous_at_path_journaled()` relocates every logical block of one non-empty regular file into a single lowest-address contiguous free physical run while preserving logical block order and file data.

The operation first recovers and checkpoints older committed WAL state, then resolves the pathname through the existing bounded symbolic-link resolver. Every current physical block must still be allocator-owned. Files that are already physically contiguous return without publishing a new transaction.

For fragmented files, every old data image is snapshotted before mutation. The replacement path reserves the complete new run before releasing any old ownership, replaces the inode's full explicit block vector, writes the copied data images, and publishes allocation plus inode metadata through one WAL transaction. A crash can therefore recover only the old mapping or the complete new mapping; partial ownership transfer is not accepted.

Deterministic crash-prefix coverage verifies allocator accounting, unique block ownership, inode references, namespace stability, data preservation, read-only fsck, journal clearing, and idempotent second recovery.

This is an allocation-time defragmentation primitive, not an on-disk extent conversion. Filesystem format v5 and the explicit inode block-vector representation are unchanged. The API does not add sparse files, reflinks, byte-level EOF semantics, background compaction, or a guarantee that future writes preserve contiguity.
