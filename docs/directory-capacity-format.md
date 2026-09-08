# Directory-capacity format planning

Format v5 stores the namespace as a checksummed directory-table image containing a concatenation of self-delimiting `DNT1` directory-entry records. The superblock already persists the directory-table reservation length, but callers previously had to convert an expected namespace size into raw metadata blocks themselves.

`format_device_for_directory_capacity` adds a bounded format-time planning surface. Callers provide a non-zero entry capacity and a non-zero worst-case directory-name length in bytes. The planner reserves enough directory-table blocks for:

- the fixed directory-table header;
- exactly the requested number of `DNT1` record headers; and
- up to the requested number of name bytes for every entry.

All arithmetic is checked before metadata initialization or superblock publication. A zero entry capacity, zero name length, a name length above the `DNT1` codec limit, arithmetic overflow, or geometry that does not fit the device is rejected as `InvalidInput`.

The guarantee is intentionally limited to directory-table bytes. The formatter retains the default inode-table reservation, so callers that also need a larger inode population or block-bearing inode capacity must use the explicit metadata-geometry formatter or another appropriate capacity planner.

This is not online directory growth or relocation. It does not introduce hashed/B-tree directories, extents, migration, or a new on-disk schema. The superblock, directory-entry codec, and directory-table image remain filesystem format v5 with their existing independently versioned record formats.

The integration regression formats for 120 entries with 80-byte names, proves the derived directory reservation exceeds the default geometry, persists and reloads the complete namespace, and requires read-only fsck to accept the resulting filesystem.
