# Pathname block-granular truncate

`truncate_file_at_path_to_blocks_journaled` composes bounded absolute pathname resolution with the existing inode-ID-based `truncate_file_to_blocks_journaled` transaction.

Before pathname resolution, it recovers and checkpoints any older committed WAL. The target inode and every followed symbolic-link component are therefore selected from recovered durable namespace state rather than a partially replayed home-write prefix. The pathname resolver then follows intermediate and final symbolic links with the repository-wide 40-expansion bound. After resolution, truncate semantics stay centralized in the existing primitive: only regular files are accepted, the target is an exact count of complete 4 KiB logical blocks, growth is rejected, and every released trailing block loses allocator ownership in the same WAL transaction that removes its inode reference.

Filesystem format v5 has no persisted byte length, so this operation does not define partial-block EOF, sparse holes, allocation-on-growth, or byte-sized truncate semantics. The on-disk format is unchanged.

Durability coverage has two layers. The existing truncate crash matrix deterministically enumerates modeled write/flush crash boundaries through a final symlink pathname and requires recovery to converge to either the complete old state or the complete truncated state. A dedicated recovery-boundary regression also interrupts creation of a pathname symlink at every modeled write/flush crash point, selects reboot images whose durable WAL already contains `Commit`, and invokes pathname truncate without an external recovery call. The truncate must recover that older transaction before resolving the path, release only the selected file's trailing blocks, preserve namespace/accounting invariants, retain unique allocator-owned physical references, pass `fsck`, checkpoint the journal to empty, and make a second recovery a no-op.

Filesystem format remains v5; this slice changes only recovery ordering at the pathname truncate boundary and introduces no on-disk encoding or migration change.
