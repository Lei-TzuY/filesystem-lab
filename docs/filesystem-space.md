# Recovered filesystem space accounting

Format v5 exposes statfs-like recovered block accounting through `filesystem_space(device, superblock)`, deterministic free-space topology through `filesystem_free_space_extents(device, superblock)`, deterministic first-fit placement queries through `filesystem_first_fit_free_extent(device, superblock, block_count)`, and bounded topology pages through `filesystem_free_space_extents_page(device, superblock, after_block, limit)`.

All queries first run the repository's normal journal recovery and checkpoint path, then run read-only fsck over the recovered home metadata before returning capacity information. This ordering matters: a committed allocation/inode update that was interrupted during home writes is completed before accounting is observed, and an allocation bitmap that disagrees with inode ownership is rejected rather than advertising a referenced block as free.

The returned `FilesystemSpace` contains:

- logical block size;
- total filesystem blocks;
- reserved metadata blocks;
- data blocks (`total - reserved`);
- allocated data blocks; and
- free data blocks.

The accounting identity is therefore `reserved + allocated_data + free_data == total`, with `data == allocated_data + free_data`.

`filesystem_free_space_extents` scans the recovered durable allocation image from the first data block through the end of the filesystem and coalesces adjacent free blocks into ascending `FreeSpaceExtent { start_block, block_count }` runs. `FilesystemFreeSpace` additionally reports the total free-block count and the largest contiguous free extent. The sum of all returned extent lengths must equal fsck's recovered free-block count or the query fails with `InvalidData`.

`filesystem_first_fit_free_extent` accepts a non-zero requested block count and returns the lowest-address free run capable of satisfying it, clipped to exactly the requested length. It deliberately uses the same ascending first-fit policy expected by the allocator-facing contiguous operations, so callers can inspect current fragmentation and placement feasibility without reimplementing topology selection. A fragmented filesystem can therefore report enough aggregate free blocks while still returning `None` when no individual run is large enough.

The first-fit result is observational only: it does not reserve blocks and creates no claim on a subsequent allocation. Any mutating operation must re-read and validate allocator ownership before publishing a transaction. A zero-block request is rejected with `InvalidInput` rather than being treated as an ambiguous trivially satisfiable placement.

`filesystem_free_space_extents_page` performs the same recovery, fsck, and complete allocator-accounting scan while bounding only the returned extent vector to `limit` entries. Its `after_block` cursor is an exclusive physical-block cursor. A cursor before the data region is clipped to the first data block; a cursor at or beyond the device returns an empty page. When a cursor lands inside a free extent, the first returned extent is clipped to begin at the next physical block, so repeated pages do not duplicate already-consumed free blocks. `next_after` is the last block of the final returned extent and is present only when another free extent remains. The page still reports whole-filesystem `total_free_blocks` and `largest_extent_blocks`, and those totals remain checked against fsck.

The extent reports are deliberately observational. They do not reserve a run, promise that a later allocation will receive one, or introduce persistent extent allocation semantics. They are useful for deterministic fragmentation measurement and future allocation-policy work while keeping the current allocator and ownership invariants explicit. Cursor pages are not mutation-stable snapshots across separate calls: each call independently recovers and validates the then-current durable filesystem state.

These surfaces deliberately do not fabricate byte-level EOF capacity, inode quotas, user quotas, sparse-block accounting, permissions, or other POSIX `statfs` fields that format v5 does not persist. They do not change the on-disk format: the superblock remains filesystem format v5 and the allocation image remains allocation-image version 1.
