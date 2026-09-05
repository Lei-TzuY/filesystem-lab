# Regular-file rename exchange

Format v5 supports one bounded exchange operation: `rename_exchange_tx::rename_exchange_files_journaled` atomically swaps the target inode identifiers of two existing regular-file namespace keys. The directory keys themselves remain fixed, while allocation metadata, inode records, file block ownership, and file data remain unchanged.

Both parent inode identifiers must name durable directories, both namespace entries must exist, and both targets must be durable regular files. The operation rejects existing fsck corruption before WAL publication. Directory exchange is intentionally excluded because exchanging directory parents requires independent cycle and ancestry semantics. Exchanging the same path, or two hard-link aliases of the same inode, is a durable no-op.

The complete resulting directory-table image is published through the existing format-v5 WAL and checkpoint path. No persisted link count, rename flag field, or on-disk format change is introduced.

The deterministic crash matrix enumerates every modeled `write_block` and `flush` boundary from journal publication through directory home replay, journal clearing, and checkpoint durability. After reboot, the namespace must be either the complete old mapping or the complete exchanged mapping; allocator ownership, inode block references, and file data must remain byte-for-byte unchanged. Recovery must converge a durable commit to the complete exchanged mapping, read-only fsck must accept the result, the journal must finish empty, and a second recovery/checkpoint pass must be a no-op.
