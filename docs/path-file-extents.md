# Pathname file extent reporting

`file_extents_at_path()` exposes the recovered physical layout of one format-v5 regular file as maximal contiguous runs.

Before reporting a mapping, the query recovers and checkpoints any committed WAL state, runs the repository-wide read-only fsck, then resolves the pathname with the existing bounded symbolic-link semantics. The resolved inode must be a regular file.

Each returned `FileExtent` contains:

- `logical_start`: the first logical block index in the run;
- `physical_start`: the first physical data block backing that run;
- `block_count`: the number of consecutive logical and physical blocks in the run.

Two neighboring logical blocks are coalesced only when the second physical block number is exactly one greater than the first. A fragmented explicit inode block vector therefore produces multiple extents. A zero-block regular file produces an empty vector.

## Consistency contract

The operation is read-only. It does not publish metadata, reserve blocks, or modify allocation placement. Full fsck validation runs before the mapping is exposed, so allocator ownership, inode block references, namespace reachability, and journal state must already agree after recovery.

## Format boundary

This API does **not** add persistent extent records. Filesystem format remains v5 and still stores an explicit physical block vector in each file inode. The returned extents are only a coalesced observation of that vector at query time; later mutations may split, merge, or reorder those runs.

The API also does not introduce sparse holes, byte-level EOF, FIEMAP compatibility, stable extent identifiers, or allocation-placement guarantees.
