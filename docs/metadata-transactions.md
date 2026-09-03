# Cross-table metadata transactions

Filesystem format v5 persists allocation, inode, and directory metadata in separate checksummed home regions, but lifecycle changes often need several regions to advance together. Creating a reachable file that immediately owns a data block is the smallest three-table example: allocation ownership, the inode block reference, and the namespace entry must describe the same committed state.

## Inode + directory transactions

`metadata_tx::store_inode_directory_tables_journaled` renders the desired inode-table and directory-table snapshots first, computes the changed home blocks across both regions, and places that complete write set into one existing WAL transaction. The durability sequence is:

1. render and validate both desired table images without touching home locations;
2. read the current durable inode and directory regions and retain only changed blocks;
3. encode one journal transaction containing every changed home block;
4. write the bounded journal image and flush it;
5. replay the committed transaction to inode/directory home locations;
6. flush the home-location writes.

A crash before the commit record is durable changes neither table. A crash after commit may leave a prefix of home writes visible, including an inode-table update without its directory-table partner, but the durable journal remains authoritative. Recovery replays the complete transaction idempotently and restores the intended cross-table state.

## Allocation + inode + directory create transactions

`create_tx::store_create_metadata_journaled` extends the same mechanism across the durable allocation bitmap, inode table, and directory table. It is intended for bounded create/link lifecycle steps where a newly allocated data block must become owned by a reachable inode in the same commit.

The desired allocator image and both metadata-table images are rendered into one capture device. Only changed blocks from the allocation, inode, and directory regions are retained. Those blocks are then framed by one Begin/Commit pair and published as one WAL transaction. The transaction is never split across commits.

For the common single-block case this prevents three inconsistent crash states from being accepted as complete operations:

- allocated-but-unreferenced data ownership;
- an inode referencing a block whose allocation bit is not durable;
- a directory entry targeting an inode whose durable lifecycle state has not advanced with it.

A crash after commit can still expose a prefix of home writes temporarily. For example, allocation and inode home blocks may be visible while the directory-table home write fails. That intermediate state is not considered complete; the committed WAL remains authoritative, and recovery must replay all home writes before fsck is expected to accept the namespace again.

## Bounded capacity

Journal records contain full 4 KiB home blocks. Transactions are never split merely to fit the reservation. If the complete changed-block set and begin/commit framing exceed the journal region, the operation returns `InvalidInput` before publishing a new journal image.

With the current record and region codecs, a transaction containing two full-block writes needs three 4 KiB journal blocks, which is why newly formatted v5 filesystems reserve three journal blocks by default. A common allocation+inode+directory create changes three home blocks and therefore needs four journal blocks. The three-table primitive deliberately exposes that capacity boundary instead of silently changing the format policy in the same milestone: callers can use explicit four-block journal geometry, while the default three-block geometry rejects the operation atomically.

Journal geometry is explicit in the v5 superblock, so future formatter-policy changes do not reinterpret existing images.

## Invariants exercised

Focused deterministic regressions verify that:

- a valid inode+directory update commits as exactly one transaction;
- an identical pair of inode/directory snapshots is a no-op;
- an uncommitted cross-table transaction mutates neither home table;
- failure on the second inode/directory home write leaves a durable transaction that recovery can replay;
- repeated recovery is idempotent;
- a too-small journal reservation rejects the combined update before home metadata changes;
- the default formatter provisions enough journal space for the common one-inode-block + one-directory-block transaction;
- a three-table create commits allocation ownership, inode references, and namespace publication as exactly one transaction when four journal blocks are reserved;
- failure on the directory home write after allocation and inode home writes is repaired by replay of the same committed three-table transaction;
- the default three-block journal rejects a three-home-block create before any home metadata changes;
- after successful recovery, read-only fsck accepts allocation ownership, inode references, root reachability, and namespace relationships.

These primitives intentionally do not yet define rename/unlink semantics, link counts, orphan handling, data-block contents, or broad POSIX behavior. Those require their own bounded lifecycle transactions and invariants.
