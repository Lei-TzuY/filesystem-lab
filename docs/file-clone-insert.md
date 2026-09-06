# Regular-file block-range clone insertion

`clone_file_blocks_insert_journaled` copies a non-empty contiguous range of complete logical blocks from an existing regular file and inserts fresh physical copies at an arbitrary logical boundary in an existing destination regular file.

The source images are read before destination metadata is mutated. Fresh physical blocks are then allocated and the following homes are published by one WAL transaction:

- allocation metadata owning every newly allocated block;
- the inode-table image containing the destination block-reference insertion;
- every freshly allocated data block containing the snapshotted source image.

The destination index is defined against the destination block vector before insertion and must be in `0..=blocks.len()`. Existing destination blocks before and after the boundary retain their order. Source and destination may name the same inode; source images and source block references are snapshotted before the insertion, so self-clone behavior is deterministic.

The operation preserves the source mapping and data, namespace, inode identities, and all pre-existing physical block ownership. Newly allocated physical blocks are owned exactly once and referenced exactly once after recovery. It does not provide reflink/shared-block semantics.

## Crash contract

Deterministic crash enumeration covers WAL publication, replay of allocation/inode/data home images, journal clearing, and checkpoint durability boundaries. Before a durable commit, recovery preserves the old filesystem state. After a durable commit, recovery converges to the complete inserted state. Raw mixed allocator/inode prefixes must not be accepted as clean filesystem states by fsck.

After successful recovery:

- every cloned destination reference names a newly allocator-owned block;
- source mappings and source data are unchanged;
- existing destination references retain their relative ordering around the inserted range;
- cloned data images exactly match the source snapshot;
- namespace invariants remain unchanged;
- fsck is clean;
- the journal is empty; and
- a second recovery/checkpoint is a no-op.

This is a format-v5 block-granular primitive. It does not define persisted byte length, EOF extension, partial-block insertion, sparse holes, extents, reflinks, or broad POSIX semantics.
