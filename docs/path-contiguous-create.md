# Contiguous pathname file creation

`create_contiguous_file_with_blocks_at_path_journaled()` creates one regular file whose initial logical blocks are placed in the allocator's lowest-numbered contiguous free physical run.

The pathname parent follows the existing bounded symbolic-link resolver. The final component must not already exist. The request must contain at least one complete 4 KiB logical block.

## Atomic publication

Before allocation, any older committed journal image is recovered and checkpointed. The operation then reserves one contiguous run and publishes the resulting allocation image, inode block vector, namespace entry, and every initial data-block image through the existing bounded create-with-data WAL transaction.

Deterministic crash-prefix tests require recovery to converge to exactly one of two states:

- the complete pre-create state, with no destination and unchanged allocation/namespace ownership; or
- the complete new file, with every requested block initialized and all physical block references forming one contiguous run.

After recovery, read-only fsck must still accept allocation accounting, unique block ownership, inode references, and namespace reachability, and a second recovery must be idempotent.

## Fragmentation contract

Contiguity is an allocation-time policy, not a new on-disk extent representation. If enough free blocks exist in aggregate but no single run is large enough, allocation fails before WAL publication and persistent state is unchanged.

Filesystem format v5 is unchanged. Inodes continue to persist explicit physical block vectors. This operation does not introduce sparse files, byte-level EOF, persistent extents, relocation, or a guarantee that later mutations preserve contiguity.
