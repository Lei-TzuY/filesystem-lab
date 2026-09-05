# Bounded regular-file zero-range

Format v5 exposes `zero_file_range_journaled` for zeroing a non-empty byte range that fits entirely inside logical blocks already referenced by a durable regular-file inode.

The operation does not change allocator ownership, inode block references, namespace metadata, or on-disk format. It does not allocate blocks, extend files, infer EOF, create sparse holes, or claim POSIX `fallocate` semantics.

Publication reuses the existing atomic cross-block byte-range WAL write path. Before commit, the durable image remains the old range. After a durable commit, recovery must converge to the complete zeroed range even if only a prefix of home data blocks was written before a crash. The fixed journal reservation is checkpointed before a successful return.

The regression matrix enumerates every modeled `write_block`/`flush` crash boundary for a range spanning two data blocks. After reboot, fsck must accept the image, recovery must expose either the complete old range or the complete zeroed range, the journal must become empty, and a second recovery/checkpoint must be a no-op.
