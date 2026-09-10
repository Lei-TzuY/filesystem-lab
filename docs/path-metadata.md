# Pathname metadata queries

Format v5 now exposes a bounded metadata surface for filesystem consumers that need `stat`/`lstat`-style pathname inspection without fabricating POSIX fields the format does not persist.

`metadata_at_path(device, superblock, path)` follows intermediate and final symbolic links. `symlink_metadata_at_path(device, superblock, path)` follows intermediate links but reports the final symbolic-link inode itself, so a dangling final symlink remains inspectable.

Both return `PathMetadata` with:

- `inode_id`: the durable inode identifier;
- `kind`: `File`, `Directory`, or `Symlink`;
- `logical_blocks`: the exact number of physical block references stored in the inode;
- `namespace_references`: the current number of durable directory entries targeting the inode.

The reference count is derived from the directory table rather than stored separately, matching the existing format-v5 hard-link model. The root inode may legitimately report zero namespace references.

Before either query resolves its pathname, it recovers and checkpoints any older committed WAL. The selected inode, any followed symbolic-link chain, and the derived namespace-reference count therefore come from recovered durable namespace state rather than a partially replayed home-write prefix. `symlink_metadata_at_path` preserves its final-component no-follow rule after that recovery boundary.

Before returning metadata, the query loads the durable allocator and rejects a resolved inode that references a reserved metadata block or an allocator-free data block. This prevents a consumer from treating an allocator/inode disagreement as trustworthy metadata.

## Recovery boundary

Deterministic crash enumeration covers both stat-style and lstat-style queries. The regression interrupts creation of a final symbolic link at every modeled write/flush crash point, selects reboot images whose durable journal contains `Commit`, and invokes the metadata query without an external recovery call. Successful queries must recover the committed namespace first, report the correct followed file or final symlink inode, preserve allocator/inode accounting, maintain unique allocator-owned physical references, pass read-only fsck, checkpoint the journal to empty, and make a second recovery a no-op.

## Deliberate boundary

Format v5 still has no persisted byte length, permissions, ownership ids, timestamps, sparse-hole map, or stored POSIX link count. These APIs therefore do **not** report a byte size or claim full `stat(2)` compatibility. `logical_blocks` and `namespace_references` are exact format-v5 facts that can be used by a future FUSE layer without inventing durability semantics.

Filesystem format remains v5. This slice changes recovery ordering at the pathname query boundary only; it does not change any on-disk encoding, journal record, or fsck rule.
