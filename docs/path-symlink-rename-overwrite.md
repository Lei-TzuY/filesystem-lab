# Pathname symbolic-link rename-overwrite

`rename_overwrite_symlink_at_path_journaled` adds one bounded rename-overwrite lifecycle for symbolic links. Source and destination parent paths use the existing bounded symbolic-link traversal rules, while neither final namespace component is followed. This permits a link inode itself to replace another link even when either persisted target is dangling.

The operation is deliberately limited to a singly linked destination symbolic-link inode. The source inode and its one-block `SYM1` payload survive unchanged under the destination name. The replaced destination namespace entry, destination inode, and exactly its owned payload block are removed together through the existing allocation+inode+directory metadata WAL transaction. A destination with multiple namespace references is rejected rather than guessing inode lifetime.

Filesystem format remains v5. No link-count field, new symlink payload encoding, byte-length semantics, orphan list, or new journal record type is introduced.

Deterministic write/flush crash enumeration verifies that recovery converges only to the complete old state or the complete replacement state. The old state retains both link inodes and both payload blocks. The replacement state retains source inode/payload ownership at the destination name while the destination inode and payload ownership are gone. In both cases namespace/inode/allocator relationships must pass read-only `fsck`, checkpoint leaves the journal empty, and a second recovery is a no-op.
