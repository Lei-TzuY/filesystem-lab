# Pathname rename-overwrite dispatch

`rename_overwrite_at_path_journaled` is the bounded POSIX-like pathname entry point for replacing an existing namespace entry without requiring callers to select a type- or link-count-specific transaction first.

## Dispatch contract

The operation validates pathname shape, recovers and checkpoints any older committed journal image, resolves only the source and destination parent pathnames through bounded symbolic-link traversal, and then inspects the named final entries without following them.

The recovered format-v5 inode kinds and destination namespace reference count select the existing durable transaction:

- regular file with one destination reference: final file rename-overwrite;
- regular file with multiple destination references: linked-destination file rename-overwrite;
- symbolic link with one destination reference: final symlink rename-overwrite;
- symbolic link with multiple destination references: linked-destination symlink rename-overwrite;
- directory: existing empty-directory rename-overwrite.

Source and destination inode kinds must match. This keeps the bounded semantics explicit rather than attempting cross-kind replacement.

## Pathname semantics

Intermediate symbolic links are followed when resolving parent paths. Final source and destination components are never followed, so a symbolic-link inode participates as the link itself even when its payload is dangling.

A single terminal slash on either operand is accepted as directory intent. If either operand has that intent, both resolved final entries must be directories because mixed kinds are rejected. Repeated trailing separators remain invalid and are rejected before durable publication.

## Durability and format

The dispatch itself performs no new on-disk encoding. Filesystem format remains **v5**. Link counts are derived from recovered directory-table references; no persisted inode link-count field is introduced.

After endpoint selection, durability is provided by the existing crash-consistent rename-overwrite transactions. Deterministic pathname crash enumeration exercises the dispatch through regular-file, symbolic-link, and directory replacement. Existing lower-level linked-destination crash matrices continue to verify the directory-only linked replacement transactions. Recovery must converge to either the complete old state or the complete committed new state, `fsck` must succeed, checkpointing must clear the journal, and a second recovery must be idempotent.
