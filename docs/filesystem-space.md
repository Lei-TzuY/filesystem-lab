# Recovered filesystem space accounting

Format v5 exposes statfs-like recovered block accounting through `filesystem_space(device, superblock)` and deterministic free-space topology through `filesystem_free_space_extents(device, superblock)`.

Both queries first run the repository's normal journal recovery and checkpoint path, then run read-only fsck over the recovered home metadata before returning capacity information. This ordering matters: a committed allocation/inode update that was interrupted during home writes is completed before accounting is observed, and an allocation bitmap that disagrees with inode ownership is rejected rather than advertising a referenced block as free.

The returned `FilesystemSpace` contains:

- logical block size;
- total filesystem blocks;
- reserved metadata blocks;
- data blocks (`total - reserved`);
- allocated data blocks; and
- free data blocks.

The accounting identity is therefore `reserved + allocated_data + free_data == total`, with `data == allocated_data + free_data`.

`filesystem_free_space_extents` scans the recovered durable allocation image from the first data block through the end of the filesystem and coalesces adjacent free blocks into ascending `FreeSpaceExtent { start_block, block_count }` runs. `FilesystemFreeSpace` additionally reports the total free-block count and the largest contiguous free extent. The sum of all returned extent lengths must equal fsck's recovered free-block count or the query fails with `InvalidData`.

The extent report is deliberately observational. It does not reserve a run, promise that a later allocation will receive one, or introduce persistent extent allocation semantics. It is useful for deterministic fragmentation measurement and future allocation-policy work while keeping the current allocator and ownership invariants explicit.

These surfaces deliberately do not fabricate byte-level EOF capacity, inode quotas, user quotas, sparse-block accounting, permissions, or other POSIX `statfs` fields that format v5 does not persist. They do not change the on-disk format: the superblock remains filesystem format v5 and the allocation image remains allocation-image version 1.
