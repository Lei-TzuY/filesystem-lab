# Configurable format-v5 metadata geometry

`format_device_with_metadata_blocks` creates a fresh format-v5 filesystem with explicit journal, inode-table, and directory-table reservations. The superblock format is unchanged: v5 already persists all three lengths, so this capability exposes existing geometry rather than silently reinterpreting disk state.

The formatter validates the complete metadata prefix before publishing block zero, initializes allocation, inode, and directory regions using that exact geometry, then writes and flushes the superblock. Zero-sized reservations and geometries that exceed the device are rejected before superblock publication.

This is intentionally a bounded directory-scaling mechanism, not dynamic directory growth. A caller can provision a larger directory-table region at format time and persist namespaces that exceed the default two-block table capacity. Runtime relocation, online resize, extents, B-tree directories, and migration of an existing filesystem remain outside this slice.

The integration regression formats a six-block directory table, stores eighty reachable regular-file entries with long names so the encoded namespace exceeds the default directory reservation, reloads the inode and directory images, and requires read-only fsck to accept the result. Reserved metadata blocks remain excluded from allocator ownership through the existing format-v5 geometry rules.
