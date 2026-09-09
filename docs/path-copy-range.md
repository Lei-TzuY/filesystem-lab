# Pathname copy-file-range

`copy_file_range_at_path_journaled` composes bounded absolute pathname resolution with the existing crash-consistent `copy_file_range_journaled` primitive.

Before resolving either endpoint, the wrapper recovers and checkpoints any older durable WAL. This ensures source and destination pathnames, including intermediate and final symbolic links, are resolved from the recovered namespace rather than a partially replayed home image. After resolution, the complete source byte range is snapshotted before the destination WAL write begins. This preserves the existing same-inode overlap semantics and makes the destination crash contract old-or-complete-new.

The operation remains deliberately bounded to blocks already referenced by durable regular-file inodes. Format v5 has no persisted byte length, so this surface does not allocate blocks, extend files, infer EOF, create sparse holes, or emulate the full POSIX/Linux `copy_file_range` contract. The on-disk format remains **v5**.

Deterministic crash tests enumerate every write/flush failure point for a cross-block copy through final symlinks. After recovery, destination bytes must be either the complete old range or the complete copied range; allocator state, inode images, and namespace remain unchanged; fsck succeeds; the checkpointed journal is empty; and a second recovery is a no-op. A separate recovery-boundary matrix crashes creation of the final source symlink, selects only reboot states whose Commit is durable, and invokes pathname copy directly without external recovery; the copy must first recover that namespace entry, preserve unique block ownership and allocator accounting, pass fsck, and checkpoint to an empty journal.
