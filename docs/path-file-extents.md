# Pathname file extent reporting

`file_extents_at_path()` exposes the recovered physical layout of one format-v5 regular file as maximal contiguous runs. `file_extents_page_at_path()` exposes the same observation through bounded pages.

Before reporting a mapping, each query recovers and checkpoints any committed WAL state, runs the repository-wide read-only fsck, then resolves the pathname with the existing bounded symbolic-link semantics. The resolved inode must be a regular file.

Each returned `FileExtent` contains:

- `logical_start`: the first logical block index in the run;
- `physical_start`: the first physical data block backing that run;
- `block_count`: the number of consecutive logical and physical blocks in the run.

Two neighboring logical blocks are coalesced only when the second physical block number is exactly one greater than the first. A fragmented explicit inode block vector therefore produces multiple extents. A zero-block regular file produces an empty vector.

## Bounded pagination

`file_extents_page_at_path()` requires a positive extent-count `limit` and accepts an optional exclusive `after_logical` cursor. Extents entirely at or before that logical block are skipped. If the cursor falls inside a physical run, the first returned extent is clipped so that it starts at the next logical block with its physical start advanced by the same number of blocks. This keeps the cursor useful even when it is not an extent boundary and avoids repeating logical blocks.

When additional extents remain beyond the bounded page, `next_after_logical` is the final logical block represented by the returned page. Passing it back advances the next call. If no additional extent remains, the continuation cursor is absent. Each call independently recovers and fsck-validates a durable snapshot, so pagination across concurrent file mutations is deliberately not a multi-call snapshot guarantee.

## Consistency contract

The operations are read-only. They do not publish metadata, reserve blocks, or modify allocation placement. Full fsck validation runs before the mapping is exposed, so allocator ownership, inode block references, namespace reachability, and journal state must already agree after recovery.

## Format boundary

These APIs do **not** add persistent extent records. Filesystem format remains v5 and still stores an explicit physical block vector in each file inode. The returned extents are only a coalesced observation of that vector at query time; later mutations may split, merge, or reorder those runs.

The APIs also do not introduce sparse holes, byte-level EOF, FIEMAP compatibility, stable extent identifiers, or allocation-placement guarantees.
