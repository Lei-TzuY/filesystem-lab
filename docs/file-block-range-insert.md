# Regular-file logical block range insertion

Format v5 exposes `insert_file_blocks_journaled` as a bounded block-granular growth primitive. It allocates one physical block for each supplied 4 KiB image and inserts those references, in order, at a caller-selected logical block boundary of an existing regular file.

## Atomicity contract

Allocator ownership, the inode block-reference vector, and every inserted data-block image are rendered before publication and committed through one WAL transaction. Existing logical blocks at and after the insertion point shift right by the number of inserted blocks. Namespace state and inode identity do not change.

A crash before durable commit preserves the old state. After durable commit, home replay may be interrupted at any write/flush boundary, but recovery must converge to the complete inserted state. Mixed allocator/inode home prefixes are not accepted as valid by fsck. Successful completion checkpoints the fixed journal reservation so it can be reused.

Deterministic crash tests enumerate the modeled mutation boundaries and require:

- inserted blocks are either all free with the old inode block list, or all owned with the complete new block list after recovery;
- no inserted physical block is double-owned;
- surviving logical references keep their original physical ownership;
- inserted data images are complete and ordered;
- namespace state remains unchanged;
- fsck succeeds after recovery;
- the journal is empty after checkpoint;
- a second recovery/checkpoint pass is a no-op.

## Deliberate limits

This operation rejects empty input and insertion indexes beyond the existing logical block count. It does not persist byte length and therefore does not define byte-level insertion, EOF movement, sparse holes, extents, or POSIX `fallocate(FALLOC_FL_INSERT_RANGE)` semantics. The on-disk format remains v5.
