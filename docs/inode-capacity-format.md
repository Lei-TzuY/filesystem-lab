# Format-v5 inode-capacity planning

`format_device_for_blockless_inode_capacity` formats a fresh v5 filesystem while deriving the inode-table reservation from a requested number of blockless inode records. This removes the need for callers to translate a namespace-scale target into raw inode-table blocks.

The guarantee is intentionally exact and bounded: the requested number of inodes fits only while those inode records have no physical block references. Each persisted block reference adds eight bytes to its inode record, so workloads that need block-bearing inode capacity should continue to use `format_device_with_metadata_blocks` with explicitly planned geometry.

The formatter retains the caller-selected journal reservation and the default directory-table reservation. It rejects zero capacity and arithmetic/geometry overflow before publishing the superblock. The persisted layout and codec versions are unchanged; filesystem format remains v5.
