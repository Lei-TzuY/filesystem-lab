# Bounded symbolic links

Filesystem format v5 supports bounded one-block symbolic-link lifecycle operations through inode-record codec v2.

`create_symlink_journaled` creates a new `Symlink` inode under an existing directory and stores one non-empty UTF-8 target string in exactly one newly allocated data block. The target is opaque filesystem data: creation does not resolve it, require that it exists, or distinguish relative from absolute paths.

The target block uses payload version 1: magic `SYM1`, a 16-bit payload version, a 16-bit byte length, an IEEE CRC-32 field, UTF-8 target bytes, and zero-filled trailing bytes. The maximum target is one block minus the 12-byte payload header.

Allocator ownership, the new symlink inode, the new namespace entry, and the complete target block image are published by one WAL transaction. A crash before durable commit must preserve the old state. A crash after durable commit may expose a prefix of home writes, but recovery must converge to the complete new state before the journal is checkpointed and reused. There is no valid mixed state in which namespace, inode ownership, allocator accounting, or target data disagree.

`read_symlink` requires a persisted symlink inode with exactly one block and validates payload magic, version, length, CRC, UTF-8, and zero trailing bytes before returning the opaque target string.

`unlink_symlink_journaled` removes the final namespace reference to one persisted symbolic link. Before WAL publication it requires a `Symlink` inode with exactly one namespace reference, validates the target payload, verifies allocator ownership of its sole target block, frees exactly that block, and removes exactly the inode and selected directory entry. Allocation, inode, and directory snapshots advance atomically through the existing unlink WAL lifecycle. Deterministic crash enumeration requires raw post-crash state to be either the complete old symlink or the complete removed state; mixed ownership/inode/namespace states must be rejected by fsck. Recovery must converge to old or new according to commit durability, clear the journal, and remain idempotent on a second recovery.

The unlink operation does not erase the freed block's old bytes. Once allocator ownership is released the block is no longer reachable filesystem data and may be reused by later allocation.

This slice intentionally does not implement pathname traversal/resolution, following symlinks during lookup, dangling-target rejection, multi-block targets, symlink hard links, rename/overwrite semantics for symlinks, permission checks, loops/depth limits, or general POSIX compatibility.
