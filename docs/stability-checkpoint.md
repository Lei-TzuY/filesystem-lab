# Durable metadata-core stability checkpoint

This document defines the consolidation boundary for the current `filesystem-lab` implementation. It is intentionally narrower than a production POSIX filesystem: the checkpoint is the set of durable metadata semantics that are already executable, crash-tested, recoverable, and checked by read-only fsck.

## Included architecture

The v5 filesystem reserves one deterministic metadata prefix:

1. superblock;
2. bounded journal;
3. allocation image;
4. inode table;
5. directory table;
6. ordinary data blocks after the metadata prefix.

The allocation, inode, and directory home regions have independent codecs and integrity checks. WAL records contain complete 4 KiB home-block images. A logical metadata operation is never split into several journal commits merely to fit the bounded reservation; insufficient capacity is an error before a new journal image is published.

## Lifecycle matrix

| Lifecycle path | Home regions | Required semantic agreement |
| --- | --- | --- |
| allocator update | allocation | owned/free accounting agrees with reserved geometry |
| inode-table update | inode | records decode and referenced blocks satisfy ownership rules |
| directory-table update | directory | keys are valid and namespace references are structurally valid |
| inode + directory update | inode, directory | namespace targets and inode lifecycle advance in one transaction |
| create | allocation, inode, directory | new ownership, inode existence, and reachability become committed together |
| unlink | allocation, inode, directory | removed namespace, inode lifecycle, and released ownership describe exactly one removal |
| rename | directory | exactly one namespace key changes while the target inode is preserved |
| truncate-to-zero | allocation, inode | the file inode survives with zero block references and exactly its prior blocks become free |

`create`, `unlink`, `rename`, and `truncate-to-zero` have deterministic integration tests that enumerate every block-device `write_block`/`flush` mutation point of a successful bounded operation.

`truncate-to-zero` is intentionally narrower than general truncate. Format v5 does not persist byte length, so the only unambiguous truncate state currently represented on disk is removal of every data-block reference while preserving the file inode and namespace. Partial-block truncation, sparse files, and file-data ordering require a separately versioned or otherwise explicitly specified data model.

## Crash-state contract

For every crash-tested lifecycle operation:

- before the journal commit becomes durable, reboot must expose the complete old durable state;
- after commit, home locations may temporarily contain a prefix of the committed writes;
- such partial home states are not accepted as a complete filesystem state when they violate cross-layer invariants;
- the durable journal is authoritative and recovery replays the complete committed write set;
- a second recovery must be idempotent and produce the same report/state;
- fsck must accept the final recovered state.

The current fault model enumerates whole-block writes and flush boundaries. It does not claim to model sector tearing, controller reordering, or partial-block writes.

## Consolidated transaction-image boundary

Metadata transaction modules render their desired table images through a shared internal `transaction_image::CaptureDevice`. The helper centralizes:

- block-count bounds;
- zero-filled reads for unwritten capture blocks;
- rendered-block extraction;
- changed-home-block comparison;
- detection of metadata encoders writing outside the transaction's declared regions.

This is an internal debt-cleanup boundary only. Public transaction APIs, WAL encoding, on-disk formats, durability ordering, and recovery semantics are unchanged by the consolidation.

## Integration gates

A change belongs inside this checkpoint only if it preserves or tightens the existing contracts. Before integration:

1. the exact candidate must pass `cargo fmt --all -- --check`;
2. clippy must pass with warnings denied;
3. the full test suite must pass;
4. persistence-ordering changes require deterministic crash/fault regressions;
5. incompatible durable layouts require an explicit new format version;
6. recovery and fsck must agree on the resulting state.

## Deferred scope

The checkpoint deliberately does not define:

- journal checkpoint/clearing or circular log metadata;
- persisted byte length, general truncate, file-data persistence ordering, sparse files, or extents;
- hard links, orphan lifecycle, recursive removal, rename overwrite/exchange;
- permissions, ACLs, symlinks, mmap, FUSE, or broad POSIX behavior;
- stronger hardware fault models such as torn sectors or storage reordering.

Those should be reopened only as separately specified, bounded milestones with their own invariants and crash model. Routine maintenance after this checkpoint should otherwise be patrol-driven: regressions, corruption acceptance, recovery/fsck disagreement, or another concrete correctness defect justify changes; repository activity by itself does not.
