# Contiguous pathname block replacement

`replace_file_blocks_contiguous_at_path_journaled` replaces one non-empty logical-block range of a pathname-resolved regular file with a non-empty caller-provided block sequence whose fresh physical blocks are allocated as one lowest-address contiguous run.

`replace_with_zeroed_blocks_contiguous_at_path_journaled` is the equal-length zero-filled replacement variant. It replaces a non-zero number of existing logical blocks with the same number of freshly allocated, independently owned physical blocks whose durable data images are all zeroes. The file's logical block count is unchanged. This is allocation-backed replacement rather than sparse zeroing, hole punching, reservation, or unwritten extents.

The pathname wrappers first recover and checkpoint any older committed WAL, then resolve intermediate and final symbolic links with the bounded pathname resolver. The inode-level operation validates the existing range and allocator ownership, reserves the complete contiguous replacement run before releasing any displaced blocks, splices the explicit inode block vector, and publishes the resulting allocation image, inode image, and replacement data blocks through one WAL transaction.

## Durability contract

Deterministic crash-prefix enumeration requires recovery to converge to exactly one of two states:

- the complete old inode block vector and old allocator ownership; or
- the complete replacement state, with every new block allocator-owned, every displaced block released, the replacement run contiguous, and all replacement data images present.

For the zero-filled variant, the complete replacement state additionally requires every newly published logical block to read back as all zeroes while the file's logical block count remains unchanged.

Namespace state is unchanged. After recovery, `fsck` must accept allocation accounting and block ownership, the journal must checkpoint empty, and a second recovery must be idempotent.

## Format boundary

This operation does **not** change the on-disk format. Filesystem format v5 continues to persist regular-file mappings as explicit block vectors; there is no extent record or migration. Contiguity is an allocation-time property of the newly inserted replacement sequence only. The operation does not define sparse files, byte-level EOF, reflinks, or POSIX byte-range replacement semantics.
