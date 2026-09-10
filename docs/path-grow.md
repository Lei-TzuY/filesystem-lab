# Pathname block-granular zero growth

`grow_file_at_path_to_blocks_journaled` grows an existing regular file to a strictly larger format-v5 logical-block count by appending newly allocated zero-filled 4 KiB blocks.

The operation first uses recovered pathname metadata to select the target inode and observe its current durable block-vector length. It then delegates one atomic allocation+inode+data publication to the existing multi-block append transaction. Existing logical blocks are neither rewritten nor released.

## Durability contract

A crash before the append commit becomes durable leaves the old block vector and allocation state. A crash after a durable commit may expose a prefix of home writes, but recovery must converge to the complete grown block vector with every newly referenced physical block allocated and zero-filled. No intermediate prefix-growth state is accepted after recovery.

Deterministic crash tests enumerate the operation's write/flush boundaries and require old-or-complete-new state, exact allocator accounting, unique allocator-owned file-block references, clean fsck, an empty checkpointed journal, and idempotent second recovery.

## Format boundary

Filesystem format remains v5. The API is intentionally block-granular because v5 does not persist byte length or partial-block EOF. It does not implement sparse holes, byte-granular `ftruncate`, extents, or implicit shrinking. The target must be strictly larger than the current logical-block count; shrinking remains the responsibility of `truncate_file_at_path_to_blocks_journaled`.
