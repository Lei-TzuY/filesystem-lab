# Regular-file rename overwrite

Format v5 has two bounded regular-file replacement operations because link count is derived from durable namespace references rather than persisted in the inode.

`rename_overwrite_file_journaled` replaces a singly linked destination. The source inode survives, while the destination inode and exactly its allocator/data ownership are removed in the same allocation + inode + directory WAL transaction.

`rename_overwrite_linked_file_journaled` replaces one namespace entry of a multiply linked destination. The source entry moves to the destination key, but the displaced destination inode, its allocated blocks, and every other alias remain unchanged. Only the directory-table image participates in this WAL transaction, followed by checkpoint clearing of the fixed journal reservation.

Both operations reject missing or non-directory parents, missing source/destination entries, non-file targets, and source/destination aliases of the same inode before WAL publication. The linked variant additionally requires at least two durable namespace references to the displaced destination; the singly linked variant requires exactly one.

The linked-destination crash matrix enumerates every modeled `write_block` and `flush` boundary through journal publication, home replay, journal clearing, and checkpoint durability. After reboot, allocator and inode ownership must be unchanged and the namespace must be either the complete old three-entry state or the complete replacement state with the surviving destination alias. A durable commit must recover to the complete replacement state, fsck must accept the result, the journal must finish empty, and a second recovery/checkpoint pass must be a no-op.

This does not change filesystem format v5, add a persisted `nlink`, permit directory hard links, or implement rename exchange semantics.
