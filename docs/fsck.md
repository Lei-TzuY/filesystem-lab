# Fsck consistency contract

`filesystem-lab` exposes a read-only consistency checker through `fsck::check_device`. Its scope deliberately matches the durable layers that exist today: the version-5 superblock, persistent allocation image, persistent inode table, persistent directory table, and bounded persistent journal region.

## Checks performed

The checker validates, in order:

1. block zero decodes as a valid version-5 superblock;
2. the superblock block count matches the opened device;
3. journal, allocation, inode-table, and directory-table reservations are contiguous, non-empty, inside the device, and form one reserved metadata prefix;
4. the allocation image has valid magic/version/flags/reserved fields, exact bitmap length, zero padding, CRC-32, zero reserved-metadata bits, and zero trailing bits;
5. the reconstructed allocator satisfies `allocated + free = data_blocks` and its in-memory executable accounting invariant;
6. the inode-table image has valid magic/version/flags, bounded payload length, exact record count, zero padding, CRC-32, unique inode IDs, and individually valid `INO1` records;
7. every inode block reference names an ordinary data block rather than reserved or out-of-range storage;
8. every inode-referenced data block is marked allocated in the durable allocation bitmap;
9. no data block is referenced by more than one inode;
10. the directory-table image has valid magic/version/flags, bounded payload length, exact record count, zero padding, CRC-32, unique `(parent, name)` keys, and individually valid `DNT1` records;
11. every directory-entry parent inode exists;
12. every directory-entry parent inode has directory kind;
13. every directory-entry target inode exists;
14. the complete journal-region image has valid magic/version/flags/reserved bytes, length, zero padding, and CRC-32;
15. every journal record decodes with its own framing/version/checksum constraints;
16. transaction ordering is structurally valid;
17. every journal write targets an ordinary data home block, allocation metadata, inode-table metadata, or directory-table metadata. Superblock and journal-reservation targets remain forbidden.

The result reports filesystem geometry, allocated/free block counts, inode record/reference counts, directory-entry count, journal entry/write counts, committed transaction count, and an optional pending transaction identifier.

The inode/allocation check is intentionally directional at this stage: every inode reference must be allocated and uniquely owned, but an allocated data block is not yet required to have an inode owner. The latter would be too strong before complete inode/file lifecycle transaction semantics define when temporarily allocated but not yet referenced blocks are legal across transactions.

The namespace check is also intentionally local rather than graph-complete. It rejects dangling parent/target inode references and non-directory parents, but it does not yet designate a root inode, require reachability from a root, reject directory cycles, or enforce link counts. Those policies need an explicit durable root/lifecycle contract rather than being inferred by fsck.

## Crash semantics

An incomplete final journal transaction is **not** corruption. It is a valid durable prefix representing a crash before the commit marker reached stable storage. The checker reports it as `pending_transaction`; recovery continues to ignore its writes.

Allocation, inode-table, and directory-table images use tail-block-first, header-block-last ordering when their direct persistence primitives are used. Their journaled mutation paths instead record changed metadata blocks in one committed WAL transaction, flush the journal, replay home blocks, and then flush home locations. A crash before commit leaves the old home image untouched; a failure after commit remains recoverable by idempotent replay of the durable journal.

`check_device` is intentionally read-only: it performs no block writes and crosses no flush boundary. Repair/checkpoint policy remains a later milestone.

## Future extension

Once root and inode/directory lifecycle semantics are explicit, fsck can extend the namespace model with root uniqueness, reachability, directory-cycle detection, link/lifecycle accounting, orphan handling, and eventually checks for leaked allocated blocks once transaction semantics make that invariant unambiguous.
