# filesystem-lab

A focused filesystem implementation and crash-consistency laboratory for building and verifying a small durable metadata stack from first principles. The project favors explicit on-disk versions, executable invariants, bounded transactions, deterministic crash testing, and read-only consistency checking over broad POSIX surface area.

## Durable metadata-core checkpoint

The current checkpoint is **filesystem format v5**. Its deterministic metadata prefix is:

`superblock -> journal -> allocation image -> inode table -> directory table -> data blocks`

The implemented core now includes:

- fixed 4 KiB logical blocks and a file-backed block device with strict bounds/size checking;
- explicit durability boundaries through the block-device `flush` contract;
- version-5 superblock geometry with independently reserved journal, allocation, inode, and directory regions;
- checksummed allocation image v1 with deterministic first-fit ownership accounting and reserved-block exclusion;
- independently versioned persisted inode records and checksummed inode-table images;
- independently versioned persisted directory entries and checksummed directory-table images;
- buffer-cache `Clean` / `Dirty` / `Writeback` state semantics and durability-aware eviction rules;
- logical WAL transactions with Begin/full-block Write/Commit records, persistent record and journal-region codecs, and committed-only replay;
- bounded journaled updates for allocation, inode, directory, inode+directory, and allocation+inode+directory metadata snapshots;
- validated atomic create, unlink, and bounded rename lifecycle operations;
- deterministic write/flush crash enumeration for create, unlink, and rename;
- idempotent recovery after committed home-write interruption;
- read-only fsck across superblock geometry, allocation ownership, journal integrity, inode block ownership, namespace references, root reachability, and directory-cycle constraints;
- focused malformed-image, corruption, insufficient-journal-capacity, and crash-prefix regressions.

The transaction paths share one internal metadata-image capture primitive so table encoders are rendered and compared with home blocks under one bounds/zero-fill contract before publication through the WAL.

## Atomicity and consistency contracts

| Operation | Durable regions advanced together | Validation before WAL publication | Exhaustive crash matrix | Post-recovery fsck |
| --- | --- | --- | --- | --- |
| allocation snapshot | allocation | geometry/capacity | focused failure tests | allocation invariants |
| inode snapshot | inode | geometry/capacity | focused failure tests | inode/ownership invariants |
| directory snapshot | directory | geometry/capacity | focused failure tests | namespace invariants |
| inode + directory | inode + directory | geometry/capacity | focused failure tests | cross-table invariants |
| create | allocation + inode + directory | bounded transaction + resulting fsck invariants | yes | yes |
| unlink | allocation + inode + directory | exact removal, reference, ownership, non-empty-directory checks | yes | yes |
| rename | directory | source/destination/parent/cycle validation | yes | yes |

A crash before a durable commit must preserve the old durable state. A crash after commit may expose a prefix of home writes, but the journal remains authoritative and repeated recovery must converge idempotently to the complete committed state. Partial cross-table states are not accepted as valid filesystem states merely because they are individually decodable.

See [`docs/metadata-transactions.md`](docs/metadata-transactions.md) and [`docs/crash-testing.md`](docs/crash-testing.md) for the executable lifecycle contracts.

## Format and recovery documentation

- [`docs/on-disk-format.md`](docs/on-disk-format.md): filesystem superblock versions and v5 metadata geometry
- [`docs/inode-record-format.md`](docs/inode-record-format.md) / [`docs/inode-table-format.md`](docs/inode-table-format.md): persisted inode representation
- [`docs/directory-entry-format.md`](docs/directory-entry-format.md) / [`docs/directory-table-format.md`](docs/directory-table-format.md): persisted namespace representation
- [`docs/journal-record-format.md`](docs/journal-record-format.md) / [`docs/journal-region-format.md`](docs/journal-region-format.md): WAL encoding
- [`docs/recovery.md`](docs/recovery.md): committed replay and durability ordering
- [`docs/fsck.md`](docs/fsck.md): read-only consistency invariants
- [`docs/stability-checkpoint.md`](docs/stability-checkpoint.md): consolidation boundary and deferred scope

Versions 1 through 4 remain documented historical schemas and are intentionally rejected by the v5 reader. Durable semantics are never silently reinterpreted across format versions.

## Checkpoint scope

This repository is now treated as a **crash-consistent durable metadata-core checkpoint**, not as an unfinished attempt to clone a production POSIX filesystem.

The following are intentionally outside the current checkpoint unless a concrete correctness requirement justifies reopening them:

- checkpointing, journal clearing, and circular journal head/tail management;
- persistent file-data write semantics, truncate, sparse files, and complex extents;
- hard-link counts, orphan handling, recursive deletion, rename overwrite/exchange semantics;
- permissions, ACLs, symlinks, mmap, FUSE integration, and broad POSIX compatibility;
- sector tearing, controller reordering, and partial-block fault models.

Future work should be event-driven: fix reproducible correctness regressions, corruption acceptance, recovery/fsck disagreement, or another narrowly specified lifecycle contract. New feature families should not be added merely to create activity.

## Development gates

Changes to durable semantics should preserve the following discipline:

1. state the invariant or lifecycle contract first;
2. version any incompatible on-disk change explicitly;
3. add focused malformed/corruption regression coverage;
4. for persistence ordering changes, add deterministic crash/fault coverage;
5. require recovery to be idempotent;
6. require read-only fsck and runtime ownership/namespace semantics to agree;
7. run formatting, clippy with warnings denied, and the complete test suite before integration.
