# Bounded regular-file range reads

Format v5 persists regular-file data ownership as an ordered list of complete 4 KiB block references in each file inode. It does **not** persist a byte length. `read_file_range` therefore exposes a bounded byte-range view only inside logical blocks that already exist.

The caller supplies a first logical block index, an offset within that block, and a non-zero byte count. A range may cross any number of existing logical block boundaries. Every touched logical block is resolved through the same inode/allocator ownership validation used by block reads. Missing or non-file inodes, ranges that extend past the inode's block list, arithmetic overflow, and references to allocator-free physical blocks are rejected.

`read_file_range_at_path` composes this same data-path contract with bounded absolute pathname resolution. It follows intermediate and final symbolic links using the existing `MAX_SYMLINK_EXPANSIONS` limit, then delegates the resolved inode to `read_file_range`. Ordinary paths, directory symlink aliases, and final symlinks to regular files therefore share one allocator/inode validation path. Dangling links and pathname lookup failures are surfaced before data access; a resolved non-file inode is rejected by the regular-file reader.

Both operations are read-only. They do not publish WAL records, allocate blocks, infer EOF, synthesize sparse holes, extend files, or change namespace/inode/allocation state. Consequently they add no new crash-consistency ordering contract and do not change the on-disk format version.

The corresponding mutation path remains `write_file_range_journaled`, which commits complete resulting block images through one WAL transaction when the entire byte range fits inside already allocated logical file blocks.
