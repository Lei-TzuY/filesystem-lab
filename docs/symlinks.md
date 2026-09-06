# Bounded symbolic links

Filesystem format v5 supports bounded one-block symbolic-link lifecycle operations through inode-record codec v2.

`create_symlink_journaled` creates a new `Symlink` inode under an existing directory and stores one non-empty UTF-8 target string in exactly one newly allocated data block. The target is opaque filesystem data: creation does not resolve it, require that it exists, or distinguish relative from absolute paths.

The target block uses payload version 1: magic `SYM1`, a 16-bit payload version, a 16-bit byte length, an IEEE CRC-32 field, UTF-8 target bytes, and zero-filled trailing bytes. The maximum target is one block minus the 12-byte payload header.

Allocator ownership, the new symlink inode, the new namespace entry, and the complete target block image are published by one WAL transaction. A crash before durable commit must preserve the old state. A crash after durable commit may expose a prefix of home writes, but recovery must converge to the complete new state before the journal is checkpointed and reused. There is no valid mixed state in which namespace, inode ownership, allocator accounting, or target data disagree.

`read_symlink` requires a persisted symlink inode with exactly one block and validates payload magic, version, length, CRC, UTF-8, and zero trailing bytes before returning the opaque target string.

`hard_link_symlink_journaled` can add another namespace alias to an existing validated symlink inode without changing allocator ownership, the inode image, or target data. `unlink_nonfinal_symlink_link_journaled` removes exactly one alias while another durable reference remains. `unlink_symlink_journaled` removes the final namespace reference, verifies ownership of the sole target block, frees exactly that block, and removes the inode and selected directory entry. Their mutation paths retain deterministic crash/recovery coverage and read-only fsck agreement.

`resolve_path_following_symlinks` adds a bounded read-only lookup surface. It accepts absolute paths rooted at inode 1, traverses directory entries component by component, and follows persisted `Symlink` inodes through `read_symlink`. Absolute symlink targets restart lookup from root; relative targets restart from the directory containing the symlink; any remaining suffix is preserved after expansion. Lookup follows at most `MAX_SYMLINK_EXPANSIONS` (40) links and rejects longer chains or loops. Missing components report `NotFound`; corrupted namespace/inode/symlink data continues to surface as consistency errors.

The lookup contract deliberately rejects empty components, trailing slashes on non-root paths, `.` and `..` rather than implementing normalization semantics. It is read-only and changes neither the WAL nor filesystem format v5.

The final-unlink operation does not erase the freed block's old bytes. Once allocator ownership is released the block is no longer reachable filesystem data and may be reused by later allocation.

This slice intentionally does not implement multi-block symlink targets, dangling-target rejection at creation time, permission checks, symlink-aware rename-overwrite semantics, `.`/`..` normalization, current-working-directory lookup, FUSE integration, or general POSIX compatibility.
