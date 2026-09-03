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

## Validated unlink transitions

`unlink_tx::store_unlink_metadata_journaled` reuses the same three-table WAL engine, but it first validates the requested post-unlink snapshots against the currently durable home metadata. Atomicity alone is not enough: without semantic validation, a caller could atomically persist a dangling namespace, leak a block, free a block still referenced elsewhere, or delete a directory that still contains children.

The current unlink primitive is deliberately narrow. Before any new journal image is published, it requires exactly one namespace entry and exactly its target inode to disappear. The root inode cannot be removed. Surviving inode records and namespace entries must remain byte-for-byte semantically unchanged, and the removed inode may have only that one durable namespace reference. A directory target must be empty before removal.

Allocator transition validation is equally strict: no new data block may become allocated, and the set of blocks changing from owned to free must equal the removed inode's complete persisted block list. This rejects partial release, unrelated release, and allocation side effects before they can cross the WAL durability boundary.

Hard links, orphan handling, recursive removal, and rename are intentionally rejected rather than approximated. Those semantics need independent lifecycle contracts before they can safely share this primitive.

## Bounded capacity

Journal records contain full 4 KiB home blocks. Transactions are never split merely to fit the reservation. If the complete changed-block set and begin/commit framing exceed the journal region, the operation returns `InvalidInput` before publishing a new journal image.

With the current record and region codecs, a transaction containing two full-block writes needs three 4 KiB journal blocks, while a common allocation+inode+directory create changes three home blocks and therefore needs four journal blocks. Newly formatted v5 filesystems reserve four journal blocks by default so the three-table atomic-create and atomic-unlink primitives are usable with ordinary `format_device()` geometry. Explicit smaller geometries remain supported for tests and constrained images; an explicit three-block journal must still reject a three-home-block transaction atomically rather than splitting the transaction.

Journal geometry is explicit in the v5 superblock, so this formatter-policy change does not reinterpret existing v5 images. A previously formatted image that persisted a three-block journal continues to use that capacity when reopened.

## Invariants exercised

Focused deterministic regressions verify that:

- a valid inode+directory update commits as exactly one transaction;
- an identical pair of inode/directory snapshots is a no-op;
- an uncommitted cross-table transaction mutates neither home table;
- failure on the second inode/directory home write leaves a durable transaction that recovery can replay;
- repeated recovery is idempotent;
- a too-small journal reservation rejects the combined update before home metadata changes;
- the default formatter provisions enough journal space for the common allocation+inode+directory create transaction;
- a three-table create commits allocation ownership, inode references, and namespace publication as exactly one transaction;
- failure on the directory home write after allocation and inode home writes is repaired by replay of the same committed three-table transaction;
- an explicit three-block journal rejects a three-home-block create before any home metadata changes;
- atomic unlink removes namespace, inode, and block ownership together;
- a committed unlink interrupted between home writes is repaired by idempotent recovery;
- unlink validation rejects a target that remains referenced, unrelated block release, and non-empty directory removal before WAL publication;
- after successful recovery, read-only fsck accepts allocation ownership, inode references, root reachability, and namespace relationships.

These primitives intentionally do not yet define rename, hard-link counts, orphan handling, recursive removal, data-block contents, or broad POSIX behavior. Those require their own bounded lifecycle transactions and invariants.
