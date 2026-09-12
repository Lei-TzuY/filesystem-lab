# Pathname whole-file zeroing

`zero_file_at_path_journaled()` atomically zeroes every persisted logical block of an existing regular file named by an absolute pathname.

The wrapper follows intermediate and final symbolic links. It first obtains recovered pathname metadata, rejects non-regular-file targets, and treats a zero-block regular file as a validated no-op. For a non-empty file it delegates the complete `logical_blocks * 4096` byte span to the existing crash-consistent pathname zero-range primitive, so one WAL publication covers the whole persisted data image.

Allocator ownership, inode block references, inode identity, and namespace entries are unchanged. Deterministic write/flush crash enumeration requires recovery to expose either the complete old data image or the complete zero image, never a mixed block state; fsck must remain clean, the journal must checkpoint empty, and a second recovery must be idempotent.

This is a format-v5 block-granular operation. The format still has no persisted byte EOF, partial final block, sparse-hole, or extent semantics, and this change requires no migration.
