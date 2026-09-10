# Regular-file block-range clone splice

`clone_file_blocks_splice_journaled` snapshots a non-empty contiguous source logical-block range and atomically replaces a non-empty existing destination logical-block range that may have a different block count.

The endpoints must be distinct regular-file inodes. Source images are read before metadata mutation. Fresh physical blocks are allocated while all displaced destination blocks remain allocator-owned; only after those allocations succeed are the displaced blocks released. One WAL transaction publishes:

- allocation metadata owning every fresh clone block and freeing exactly the displaced destination blocks;
- the destination inode block vector with the requested range replaced by the fresh references, including any resulting growth or shrinkage;
- the complete cloned data images for every fresh home block.

Source mappings/data, namespace entries, inode identities, and unaffected destination ordering remain unchanged. The operation never reuses a displaced destination home as one of its fresh clone blocks.

`clone_file_blocks_splice_at_path_journaled` exposes the same primitive through bounded absolute-path resolution. Before either endpoint is resolved, it recovers and checkpoints any older committed WAL transaction. Intermediate and final symbolic links are then followed for both endpoints, so source and destination inode selection observes the recovered namespace before the resolved inode IDs are passed to the inode-level operation. The pathname surface adds no second durability mechanism or on-disk state.

## Crash contract

Deterministic write/flush fault enumeration covers WAL publication, metadata/data home replay, journal clearing, and checkpoint durability boundaries. Before recovery, a durable image is accepted only when allocator ownership and inode references describe either the complete old state or the complete new splice state; mixed allocation/inode states must fail read-only fsck. After recovery, committed transactions converge to the complete new state, uncommitted transactions preserve the old state, the journal is empty, and a second recovery/checkpoint is a no-op.

The pathname crash matrix additionally requires source data and namespace state to remain unchanged across every interruption while allocator and inode images recover to exactly the old or complete new state. A separate recovery-boundary matrix enumerates crashes while creating a final source symlink, selects reboot images with a durable Commit, and invokes pathname clone-splice without an external recovery call. The operation must first replay/checkpoint that namespace transaction, resolve the recovered symlink, publish the splice, preserve unique allocator-owned physical references, pass fsck, leave an empty journal after checkpoint, and make a second recovery a no-op.

For a splice replacing `D` destination blocks with `S` source clones, successful accounting changes by `S - D` allocated blocks. No physical block may be referenced twice.

## Scope

This remains filesystem format v5 and is intentionally block-granular. There is no persisted byte length, so this API does not define EOF growth/shrinkage, partial-block splice behavior, sparse holes, extents, reflinks, or POSIX `fallocate` semantics.
