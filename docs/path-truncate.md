# Pathname block-granular truncate

`truncate_file_at_path_to_blocks_journaled` composes bounded absolute pathname resolution with the existing inode-ID-based `truncate_file_to_blocks_journaled` transaction.

The pathname resolver follows intermediate and final symbolic links with the repository-wide 40-expansion bound. After resolution, truncate semantics stay centralized in the existing primitive: only regular files are accepted, the target is an exact count of complete 4 KiB logical blocks, growth is rejected, and every released trailing block loses allocator ownership in the same WAL transaction that removes its inode reference.

Filesystem format v5 has no persisted byte length, so this operation does not define partial-block EOF, sparse holes, allocation-on-growth, or byte-sized truncate semantics. The on-disk format is unchanged.

Durability coverage deterministically enumerates modeled write/flush crash boundaries through a final symlink pathname. After recovery, the filesystem must be either the complete old state or the complete truncated state; no released block may remain referenced, no retained block may lose ownership, the namespace must remain unchanged, `fsck` must pass, the journal must checkpoint empty, and a second recovery must be a no-op.
