# Pathname empty regular-file creation

Format v5 exposes `create_empty_file_at_path_journaled` as a bounded pathname-facing create surface for zero-block regular files.

The destination must be absolute and contain a non-empty final component. The operation splits the destination into parent pathname and basename, resolves the parent with the existing bounded symbolic-link-following resolver, requires that resolved inode to be a durable directory, rejects an existing child with the same name, assigns a fresh non-zero inode id, and publishes one new `File` inode plus one directory entry through the existing create WAL transaction.

The new inode starts with an empty logical-block vector. Format v5 does not persist byte length, so this slice deliberately means only “a reachable regular-file inode that owns zero data blocks”; it does not claim POSIX byte-length/EOF, permissions, timestamps, open flags, sparse allocation, or `O_CREAT` semantics.

Because the new inode owns no blocks, allocator state must remain byte-for-byte unchanged. Inode-table and directory-table images advance atomically: a crash before durable commit recovers the complete old namespace, while a crash after durable commit recovers the complete new inode plus namespace entry. Deterministic write/flush crash enumeration verifies old-or-new recovery, no duplicate physical ownership, unchanged allocated/free accounting, fsck cleanliness, journal checkpointing, and idempotent second recovery.

No on-disk codec, superblock geometry, recovery ordering, allocator format, inode record format, or directory-entry format changes in this slice. Filesystem format remains v5.
