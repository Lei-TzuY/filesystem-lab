# Bounded copy-file-range

Format v5 now exposes a bounded `copy_file_range_journaled` operation over blocks that are already referenced by durable regular-file inodes.

The operation first reads the complete source byte range into memory and only then publishes the destination update through the existing cross-block WAL write path. That ordering gives overlapping copies within the same inode snapshot semantics: destination writes cannot alter bytes that have not yet been read from the source.

Both source and destination ranges must fit entirely within existing logical blocks. The operation does not allocate blocks, extend files, infer EOF, create sparse holes, or add a persisted byte length. The on-disk format remains v5.

Durability is inherited from the existing atomic cross-block destination write transaction. A crash during source reads has no persistent effect. Once destination publication begins, the same deterministic write/flush crash model used by cross-block range writes applies: recovery must expose either the old destination range or the complete copied range, never a committed prefix. Allocator ownership, inode block references, and namespace metadata are unchanged by this operation.
