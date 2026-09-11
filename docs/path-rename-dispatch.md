# POSIX-like pathname rename dispatch

`rename_posix_at_path_journaled` provides one recovered pathname surface for the common rename contract instead of requiring callers to choose the no-replace or overwrite primitive in advance.

The dispatcher validates pathname shape, recovers and checkpoints any older committed WAL, and resolves only source and destination parent paths through bounded symbolic-link traversal. Final components are never followed.

After recovery it inspects the namespace:

- if the destination is absent, it delegates to the existing crash-consistent ordinary rename;
- if the destination names a different inode, it delegates to the existing recovered rename-overwrite dispatcher, including its same-kind and linked-destination rules;
- if source and destination already name the same inode, the rename is a no-op, including hard-link aliases.

A single terminal slash remains directory intent and repeated trailing separators remain invalid. Directory intent is checked even when two names already alias the same inode, so a regular-file alias cannot bypass pathname type requirements through the no-op branch.

## Durability contract

Deterministic crash enumeration exercises both mutating branches. After every modeled write/flush interruption, recovery must converge to either the complete pre-rename namespace or the complete committed rename state. The tests also require allocator/inode ownership consistency, read-only fsck acceptance, an empty journal after checkpoint, and idempotent second recovery.

The same-inode branch publishes no new WAL transaction after recovery/checkpoint.

## On-disk compatibility

This surface does not change the filesystem encoding. The filesystem remains **format v5**; no migration is required. Link multiplicity continues to be derived from recovered directory references rather than a persisted inode link-count field.
