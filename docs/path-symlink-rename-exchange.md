# Pathname symbolic-link rename exchange

`rename_exchange_symlinks_at_path_journaled` adds one bounded rename-exchange surface for symbolic-link inodes. Parent pathnames use the existing bounded symbolic-link traversal rules, while neither final namespace component is followed. This permits two link inodes to exchange names even when either persisted target is dangling.

The operation delegates durable publication to `rename_exchange_symlinks_journaled`. Only the directory table advances through the existing WAL transaction; both symlink inodes, their one-block `SYM1` payloads, and allocator ownership remain unchanged. Exchanging the same namespace key or two hard-link aliases of the same symlink inode is a durable no-op.

Filesystem format remains v5. No new journal record, link-count field, symlink payload encoding, or inode layout is introduced.

Deterministic write/flush crash enumeration verifies old-or-complete-new namespace recovery, exact allocator and inode preservation, stable symlink payloads, read-only fsck cleanliness, checkpoint-empty journal state, and idempotent second recovery.
