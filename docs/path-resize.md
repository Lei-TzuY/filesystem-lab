# Pathname block resize

`resize_file_at_path_to_blocks_journaled` provides one bidirectional, pathname-addressed size-control surface for format-v5 regular files.

The operation is deliberately block-granular because format v5 does not persist a byte length. A successful resize therefore changes only the inode's vector of complete 4 KiB logical blocks; it does not define partial-block EOF, sparse holes, or byte-granular POSIX `ftruncate` semantics.

Before choosing a direction, pathname metadata lookup recovers and checkpoints any older committed WAL. Final and intermediate symbolic links use the existing pathname resolution contract.

- Growth delegates to the existing zero-growth path. Fresh physical blocks are allocated, zero-filled, and published with allocator and inode state under the existing WAL transaction.
- Shrink delegates to the existing truncate transaction. The exact trailing physical block suffix is removed from the inode and released from allocator ownership atomically.
- Equal-size requests are rejected so every successful call represents an actual durable transition.

The returned physical-block vector contains blocks whose ownership changed: newly allocated blocks when growing and released trailing blocks when shrinking.

## Crash and consistency contract

The resize surface inherits the underlying allocation+inode(+data for growth) transaction boundaries. Deterministic crash enumeration covers the new pathname growth entry point and requires recovery to converge to either the complete old state or the complete resized state. Existing truncate crash coverage continues to exercise the shrink transaction directly.

After recovery the allocator and inode table must agree, every referenced physical block must be uniquely owned, fsck must remain clean, checkpointing must leave an empty journal, and a second recovery pass must be a no-op.

## On-disk compatibility

No persisted structure changes. Superblock, inode, allocator, directory, journal, and data encodings remain filesystem format v5; no migration is required.
