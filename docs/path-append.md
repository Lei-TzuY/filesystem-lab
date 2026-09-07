# Pathname multi-block append

`append_file_blocks_at_path_journaled` composes bounded absolute pathname resolution with the existing inode-ID-based `append_file_blocks_journaled` transaction.

Intermediate and final symbolic links are followed with the repository-wide 40-expansion limit. After resolution, append semantics stay centralized in the existing primitive: only regular files are accepted, at least one complete 4 KiB block is required, fresh physical blocks are allocated deterministically, and allocator ownership, inode block references, and every appended data image are published in one WAL transaction.

Filesystem format v5 has no persisted byte length, so this operation deliberately does not define partial-block EOF, sparse holes, byte-granular extension, or extents. The on-disk format is unchanged.

Durability coverage deterministically enumerates modeled write/flush crash boundaries through a final symlink pathname. After recovery, the filesystem must be either the complete old state or the complete appended state; allocator accounting must match inode ownership with no duplicate references, namespace state must remain unchanged, `fsck` must pass, the journal must checkpoint empty, and a second recovery must be a no-op.
