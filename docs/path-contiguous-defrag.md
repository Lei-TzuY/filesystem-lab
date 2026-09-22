# Contiguous pathname file defragmentation

`defragment_file_contiguous_at_path_journaled()` relocates every logical block of one non-empty regular file into a single lowest-address contiguous free physical run while preserving logical block order and file data.

`defragment_file_range_contiguous_at_path_journaled()` applies the same durability contract to one non-empty logical-block subrange. Blocks before and after the selected range keep their existing physical mappings, while the selected data is snapshotted and relocated into one fresh contiguous run without changing logical positions or file length. A selected range that is already physically contiguous is an idempotent no-op.

Both operations first recover and checkpoint older committed WAL state, then resolve the pathname through the existing bounded symbolic-link resolver. Every physical block selected for relocation must still be allocator-owned.

For fragmented selections, every displaced data image is snapshotted before mutation. The replacement path reserves the complete new run before releasing any displaced ownership, replaces only the selected portion of the inode's explicit block vector, writes the copied data images, and publishes allocation plus inode metadata through one WAL transaction. A crash can therefore recover only the old mapping or the complete new mapping; partial ownership transfer is not accepted.

Deterministic crash-prefix coverage verifies allocator accounting, unique block ownership, unchanged out-of-range inode references, namespace stability, data preservation, read-only fsck, journal clearing, and idempotent second recovery.

This is an allocation-time defragmentation primitive, not an on-disk extent conversion. Filesystem format v5 and the explicit inode block-vector representation are unchanged. The APIs do not add sparse files, reflinks, byte-level EOF semantics, background compaction, or a guarantee that future writes preserve contiguity.
