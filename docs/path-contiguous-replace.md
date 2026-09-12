# Contiguous pathname block replacement

`replace_file_blocks_contiguous_at_path_journaled` replaces one non-empty logical-block range of a pathname-resolved regular file with a non-empty caller-provided block sequence whose fresh physical blocks are allocated as one lowest-address contiguous run.

The pathname wrapper first recovers and checkpoints any older committed WAL, then resolves intermediate and final symbolic links with the bounded pathname resolver. The inode-level operation validates the existing range and allocator ownership, reserves the complete contiguous replacement run before releasing any displaced blocks, splices the explicit inode block vector, and publishes the resulting allocation image, inode image, and replacement data blocks through one WAL transaction.

## Durability contract

Deterministic crash-prefix enumeration requires recovery to converge to exactly one of two states:

- the complete old inode block vector and old allocator ownership; or
- the complete replacement state, with every new block allocator-owned, every displaced block released, the replacement run contiguous, and all replacement data images present.

Namespace state is unchanged. After recovery, `fsck` must accept allocation accounting and block ownership, the journal must checkpoint empty, and a second recovery must be idempotent.

## Format boundary

This operation does **not** change the on-disk format. Filesystem format v5 continues to persist regular-file mappings as explicit block vectors; there is no extent record or migration. Contiguity is an allocation-time property of the newly inserted replacement sequence only. The operation does not define sparse files, byte-level EOF, reflinks, or POSIX byte-range replacement semantics.
