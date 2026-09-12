# Recovered filesystem space accounting

Format v5 now exposes a small statfs-like read surface through `filesystem_space(device, superblock)`.

The query first runs the repository's normal journal recovery and checkpoint path, then runs read-only fsck over the recovered home metadata before returning any capacity values. This ordering matters: a committed allocation/inode update that was interrupted during home writes is completed before accounting is observed, and an allocation bitmap that disagrees with inode ownership is rejected rather than advertising a referenced block as free.

The returned `FilesystemSpace` contains:

- logical block size;
- total filesystem blocks;
- reserved metadata blocks;
- data blocks (`total - reserved`);
- allocated data blocks; and
- free data blocks.

The accounting identity is therefore `reserved + allocated_data + free_data == total`, with `data == allocated_data + free_data`.

This surface deliberately does not fabricate byte-level EOF capacity, inode quotas, user quotas, sparse-block accounting, permissions, or other POSIX `statfs` fields that format v5 does not persist. It also does not change the on-disk format: the superblock remains filesystem format v5 and the allocation image remains allocation-image version 1.
