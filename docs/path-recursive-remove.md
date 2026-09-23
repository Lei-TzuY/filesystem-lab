# Crash-consistent recursive subtree removal

`path_unlink::remove_directory_tree_at_path_journaled` removes one non-root directory subtree in a
single bounded filesystem transaction.

The final pathname component is not followed and must name a directory. Intermediate components use
the normal bounded symlink resolver. A single trailing slash is accepted with the same no-follow
final-component rule as ordinary directory removal.

## Namespace and ownership policy

The operation starts only from checked recovered state that passes strict fsck. It discovers the
directory closure rooted at the selected target and removes:

- the selected parent/name entry;
- every namespace entry whose parent directory lies inside that closure.

Directory hard-link ambiguity is fail-closed: if any directory in the subtree also has a namespace
reference outside the removed entry set, the operation returns before WAL publication.

Regular files and symbolic links are reference-aware. Removing subtree entries does not retire an
inode while another namespace entry outside the subtree still targets it. An inode and its owned
blocks are removed only when recursive removal eliminates its final durable namespace reference.
Existing file/symlink contents are otherwise unchanged.

Before publication, the desired inode/directory snapshot must pass the same strict namespace
validator used by fsck. Starting strict fsck plus exact release of blocks owned by retired inodes keeps
allocator/inode ownership synchronized.

## Durability contract

The desired allocation bitmap, inode table, and directory table are rendered and published through
one bounded WAL transaction. The operation is never decomposed into a sequence of per-child unlink
transactions. If the complete changed metadata image does not fit the bounded journal, the operation
fails before a replacement transaction is published.

Deterministic crash testing enumerates every modeled `write_block` / `flush` point of a successful
recursive removal. After reboot and journal recovery, the filesystem must be either the complete old
tree or the complete removed state; strict fsck must accept either state. Retrying from the old state
converges to the same final removed state.

The operation does not add persisted link counts, sparse files, or an unbounded/circular journal.
