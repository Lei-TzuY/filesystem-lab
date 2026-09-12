# Pathname whole-file block exchange

`exchange_complete_files_at_path_journaled()` atomically exchanges the complete persisted logical-block vectors of two distinct regular files selected by pathname.

Both paths are resolved only after recovery/checkpoint of older committed WAL, and both follow intermediate and final symbolic links using the existing bounded pathname rules. The resolved inode IDs are handed to `exchange_complete_file_blocks_journaled()`, which validates regular-file kind, allocator ownership, duplicate/shared physical references, and then publishes one inode-table WAL transaction.

The operation swaps physical block references rather than copying block contents. Allocator ownership and namespace state are unchanged. Either file may be empty, so empty↔non-empty exchange is supported atomically; exchanging two empty files is a validated no-op with no WAL publication.

This remains filesystem format v5. A "whole file" is exactly the inode's persisted sequence of complete 4 KiB logical blocks. The operation does not add byte EOF, partial-final-block, sparse-hole, extent, reflink/COW, or broader POSIX semantics.

Deterministic crash-prefix tests enumerate the modeled write/flush interruption points for an empty↔non-empty exchange. After reboot and recovery, the inode table must equal either the complete old state or the complete exchanged state; allocator and namespace images remain unchanged, `fsck` is clean, the journal checkpoints empty, and a second recovery is idempotent.
