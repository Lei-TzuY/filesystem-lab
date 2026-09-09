# Pathname multi-block append

`append_file_blocks_at_path_journaled` composes bounded absolute pathname resolution with the existing inode-ID-based `append_file_blocks_journaled` transaction.

Before resolving the pathname, the wrapper recovers and checkpoints any older durable WAL. This ordering prevents a new append from selecting its target inode from a partially replayed namespace after a crash. Intermediate and final symbolic links are then followed with the repository-wide 40-expansion limit. After resolution, append semantics stay centralized in the existing primitive: only regular files are accepted, at least one complete 4 KiB block is required, fresh physical blocks are allocated deterministically, and allocator ownership, inode block references, and every appended data image are published in one WAL transaction.

Filesystem format v5 has no persisted byte length, so this operation deliberately does not define partial-block EOF, sparse holes, byte-granular extension, or extents. The on-disk format is unchanged.

Durability coverage includes two deterministic crash matrices. The append transaction itself is enumerated through a final symlink pathname and must recover to either the complete old state or the complete appended state. A second matrix crashes creation of the final symlink after durable commit and then invokes pathname append without an external recovery call; append must recover/checkpoint the older WAL before resolving the symlink and deriving the target inode. Across both matrices, allocator accounting must match inode ownership with no duplicate references, namespace state must converge, `fsck` must pass, the journal must checkpoint empty, and a second recovery must be a no-op.
