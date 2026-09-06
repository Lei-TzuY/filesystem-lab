# Bounded symbolic links

Filesystem format v5 supports one bounded symbolic-link lifecycle through inode-record codec v2.

`create_symlink_journaled` creates a new `Symlink` inode under an existing directory and stores one non-empty UTF-8 target string in exactly one newly allocated data block. The target is opaque filesystem data: creation does not resolve it, require that it exists, or distinguish relative from absolute paths.

The target block uses payload version 1: magic `SYM1`, a 16-bit payload version, a 16-bit byte length, an IEEE CRC-32 field, UTF-8 target bytes, and zero-filled trailing bytes. The maximum target is one block minus the 12-byte payload header.

Allocator ownership, the new symlink inode, the new namespace entry, and the complete target block image are published by one WAL transaction. A crash before durable commit must preserve the old state. A crash after durable commit may expose a prefix of home writes, but recovery must converge to the complete new state before the journal is checkpointed and reused. There is no valid mixed state in which namespace, inode ownership, allocator accounting, or target data disagree.

`read_symlink` requires a persisted symlink inode with exactly one block and validates payload magic, version, length, CRC, UTF-8, and zero trailing bytes before returning the opaque target string.

This slice intentionally does not implement pathname traversal/resolution, following symlinks during lookup, dangling-target rejection, multi-block targets, permission checks, loops/depth limits, or general POSIX compatibility.
