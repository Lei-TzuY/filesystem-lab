# Pathname rename-exchange dispatch

`rename_exchange_at_path_journaled` is the bounded POSIX-like pathname entry point for exchanging two existing same-kind namespace entries without requiring callers to choose a file-, symlink-, or directory-specific primitive first.

The operation validates pathname shape, recovers and checkpoints any older committed journal image, resolves only the parent pathnames through bounded symbolic-link traversal, and inspects the named final entries without following them. Recovered inode kinds then dispatch to the existing regular-file, symbolic-link, or directory exchange transaction. Mixed kinds are rejected before publication.

A single terminal slash on either operand expresses directory intent. Repeated trailing separators remain invalid, and directory intent requires both final entries themselves to be directories; a final symlink is never followed merely because `/` was present.

Exchange remains a directory-only WAL mutation. File data, symlink payload blocks, inode records, and allocator ownership are unchanged by successful publication. Crash testing requires recovery to yield either the complete old namespace or the complete exchanged namespace, with allocator/inode ownership preserved, `fsck` clean, the checkpointed journal empty, and a second recovery idempotent.

No persisted layout changes are introduced. The filesystem format remains v5 and no migration is required.
