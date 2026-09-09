# Pathname rename-overwrite for multiply linked symbolic links

`rename_overwrite_linked_symlink_at_path_journaled` replaces one selected namespace alias of a multiply linked symbolic-link destination with a source symbolic link.

Source and destination parent pathnames use the existing bounded symbolic-link resolver, while both final components remain unfollowed. The operation therefore acts on symbolic-link inodes themselves, including dangling links.

The destination must have at least two namespace references. Only the selected destination entry is removed; its remaining aliases keep the destination inode alive, so its `SYM1` payload block and allocator ownership remain unchanged. The source inode and payload also remain unchanged and are published under the destination name. Because the resulting durable mutation is directory-only, the existing journaled directory-table transaction is reused rather than inventing a second lifecycle path.

The filesystem format remains v5. Link count continues to be derived from namespace references; no persisted link-count field or migration is introduced.

Deterministic crash enumeration interrupts every modeled write/flush boundary. Recovery must produce either the complete old namespace or the complete replacement namespace. In both outcomes allocator and inode images remain exact, no block ownership changes, `fsck` passes, checkpoint leaves an empty journal, and a second recovery is a no-op.
