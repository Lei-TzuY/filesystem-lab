# Pathname regular-file rename exchange

`rename_exchange_files_at_path_journaled` exposes the existing format-v5 regular-file namespace exchange through bounded absolute pathname resolution.

Both pathnames are split into parent path and final component. Parent paths follow the existing bounded symbolic-link traversal rules; final components are not followed. The resolved directory keys are passed to `rename_exchange_files_journaled`, so validation and durable publication remain centralized in the existing directory-only WAL transaction.

The operation exchanges only existing regular-file namespace targets. File inodes, data blocks, and allocator ownership remain unchanged. Exchanging the same namespace key or two hard-link aliases of the same inode remains a durable no-op.

Deterministic write/flush fault enumeration verifies old-or-complete-new namespace recovery, unchanged allocator and inode images, read-only fsck acceptance, journal checkpointing, and idempotent second recovery.

This operation does not change the on-disk schema. The filesystem remains format v5.
