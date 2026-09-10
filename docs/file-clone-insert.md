# Regular-file block-range clone insertion

`clone_file_blocks_insert_journaled` copies a non-empty contiguous range of complete logical blocks from an existing regular file and inserts fresh physical copies at an arbitrary logical boundary in an existing destination regular file.

The source images are read before destination metadata is mutated. Fresh physical blocks are then allocated and the following homes are published by one WAL transaction:

- allocation metadata owning every newly allocated block;
- the inode-table image containing the destination block-reference insertion;
- every freshly allocated data block containing the snapshotted source image.

The destination index is defined against the destination block vector before insertion and must be in `0..=blocks.len()`. Existing destination blocks before and after the boundary retain their order. Source and destination may name the same inode; source images and source block references are snapshotted before the insertion, so self-clone behavior is deterministic.

The operation preserves the source mapping and data, namespace, inode identities, and all pre-existing physical block ownership. Newly allocated physical blocks are owned exactly once and referenced exactly once after recovery. It does not provide reflink/shared-block semantics.

## Pathname surface

`clone_file_blocks_insert_at_path_journaled` exposes the same transaction through bounded absolute-path resolution. Before resolving either endpoint it recovers and checkpoints any older committed WAL, so source and destination inode selection cannot observe a partially replayed namespace. Source and destination then follow intermediate and final symbolic links, and the resolved inode IDs are delegated to the inode-ID primitive together with the destination insertion boundary. The wrapper adds no independent allocation or data-publication logic.

Missing or dangling paths, non-file endpoints, empty or out-of-range source intervals, an insertion boundary outside `0..=destination.blocks.len()`, allocator exhaustion, ownership disagreement, journal-capacity failures, and recovery/checkpoint I/O failures are rejected before a successful publication can be reported.

## Crash contract

Deterministic crash enumeration covers WAL publication, replay of allocation/inode/data home images, journal clearing, and checkpoint durability boundaries. Before a durable commit, recovery preserves the old filesystem state. After a durable commit, recovery converges to the complete inserted state. Raw mixed allocator/inode prefixes must not be accepted as clean filesystem states by fsck.

The pathname surface additionally enumerates crashes while publishing a source symlink and retries clone-insert directly from rebooted states whose older WAL already contains a durable commit. The retry must first recover that namespace transaction and only then resolve the source and destination paths.

After successful recovery:

- every cloned destination reference names a newly allocator-owned block;
- source mappings and source data are unchanged;
- existing destination references retain their relative ordering around the inserted range;
- cloned data images exactly match the source snapshot;
- namespace invariants remain unchanged;
- physical file-block references have no duplicate ownership;
- fsck is clean;
- the journal is empty; and
- a second recovery/checkpoint is a no-op.

This is a format-v5 block-granular primitive. It does not define persisted byte length, EOF extension, partial-block insertion, sparse holes, extents, reflinks, or broad POSIX semantics.
