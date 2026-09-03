# Cross-table metadata transactions

Filesystem format v5 persists inode and directory tables in separate checksummed home regions, but namespace lifecycle changes often need both tables to advance together. Creating a reachable inode is the smallest example: publishing the inode without its directory entry leaves an unreachable inode, while publishing the directory entry without the inode leaves a dangling namespace target.

`metadata_tx::store_inode_directory_tables_journaled` renders the desired inode-table and directory-table snapshots first, computes the changed home blocks across both regions, and places that complete write set into one existing WAL transaction. The durability sequence is:

1. render and validate both desired table images without touching home locations;
2. read the current durable inode and directory regions and retain only changed blocks;
3. encode one journal transaction containing every changed home block;
4. write the bounded journal image and flush it;
5. replay the committed transaction to inode/directory home locations;
6. flush the home-location writes.

A crash before the commit record is durable changes neither table. A crash after commit may leave a prefix of home writes visible, including an inode-table update without its directory-table partner, but the durable journal remains authoritative. Recovery replays the complete transaction idempotently and restores the intended cross-table state.

## Bounded capacity

Journal records contain full 4 KiB home blocks. The transaction is never split merely to fit the reservation. If the complete inode+directory changed-block set and its begin/commit framing exceed the journal region, `store_inode_directory_tables_journaled` returns `InvalidInput` before publishing a new journal image.

With the current record and region codecs, a transaction containing two full-block writes needs more than two 4 KiB journal blocks. Format v5 stores journal geometry explicitly, so filesystems provisioned with at least three journal blocks can execute the common one-inode-block + one-directory-block transaction without changing the on-disk format version. The default formatter still reserves two journal blocks; changing that default is intentionally separate from this milestone.

## Invariants exercised

Focused deterministic regressions verify that:

- a valid inode+directory update commits as exactly one transaction;
- an identical pair of snapshots is a no-op;
- an uncommitted cross-table transaction mutates neither home table;
- failure on the second home write leaves a durable transaction that recovery can replay;
- repeated recovery is idempotent;
- a too-small journal reservation rejects the combined update before home metadata changes;
- after successful recovery, read-only fsck accepts the root, reachability, and namespace relationships.

This primitive intentionally does not include allocation changes, rename/unlink semantics, link counts, orphan handling, or broad POSIX behavior. Those require their own bounded lifecycle transactions and invariants.
