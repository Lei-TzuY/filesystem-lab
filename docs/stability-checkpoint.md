# Durable metadata-core stability checkpoint

This document defines the consolidation boundary for the current `filesystem-lab` implementation. It is intentionally narrower than a production POSIX filesystem: the checkpoint is the set of durable metadata semantics that are already executable, crash-tested, recoverable, and checked by read-only fsck.

## Included architecture

The v6 filesystem reserves one deterministic metadata prefix:

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
| exact-byte grow | allocation, inode, data | EOF advances atomically, only required trailing blocks become owned, and every newly visible byte is zero |
| extending byte write | allocation, inode, data | gap zero-fill, payload, ownership, block mapping, and new EOF become committed together |
| recursive subtree remove | allocation, inode, directory | subtree namespace edges disappear together; only last-reference non-directory inodes retire; external directory references fail closed |

`create`, `unlink`, `rename`, and `truncate-to-zero` have deterministic integration tests that enumerate every block-device `write_block`/`flush` mutation point of a successful bounded operation.

`truncate-to-zero` remains as a block-granular compatibility path. Format v6 additionally persists exact regular-file byte EOF and supports crash-consistent exact-byte resize in both directions: shrink zeroes discarded tail bytes and releases only the trailing block suffix, while grow zero-fills the newly visible range and allocates only the required trailing blocks. Sparse extension remains separately deferred.

## Crash-state contract

For every crash-tested lifecycle operation:

- before the journal commit becomes durable, reboot must expose the complete old durable state;
- after commit, home locations may temporarily contain a prefix of the committed writes;
- such partial home states are not accepted as a complete filesystem state when they violate cross-layer invariants;
- the durable journal is authoritative and recovery replays the complete committed write set;
- a second recovery must be idempotent and produce the same report/state;
- fsck must accept the final recovered state.

The current fault model enumerates whole-block writes and flush boundaries. Journal publication and checkpoint also run under a write-through variant where each successful whole-block write may become durable before the next flush, matching the one-way guarantee of the `BlockDevice` flush contract. The model still does not claim sector tearing, controller reordering, or partial-block persistence.

Since the original v5 checkpoint, the executable surface has expanded beyond the initial lifecycle table: journal checkpoint/clearing, hard links, symbolic links, rename overwrite/exchange families, block-granular file range operations, strict bidirectional allocator/inode ownership checks, bounded orphan-allocation repair, crash-consistent namespace-orphan reattachment, and read-only semantic recovery projection are now implemented and covered by the repository's integration gates. High-level regular-file, metadata, and namespace pathname operations additionally use fail-closed checked recovery so a structurally valid WAL that projects to an inconsistent complete filesystem is rejected before home replay. Namespace coverage includes create variants, directory observation, hard-link/symlink lifecycle, rename dispatch/overwrite/exchange, and unlink/rmdir surfaces.

Durable inode construction and block-count mutation now also have explicit invariant boundaries. New production inode values use `PersistedInode::new`; operations that grow, shrink, or transfer logical block counts use `replace_block_range`, which validates the complete candidate mapping before committing the in-memory inode change. That consolidation now feeds format-v6 byte EOF: block-count changes preserve final-block tail slack, while explicit exact-byte shrink updates EOF through the same durable inode boundary.

The bounded journal region now writes version-2 empty/active anchors. Tail blocks are staged behind a durable empty anchor and flushed before active publication; checkpoint invalidates the log with one checksummed empty-anchor block after home replay is durable. Complete version-1 journal images remain readable. This closes the earlier dependence on pre-flush writes being volatile and remains part of the format-v6 durability model.

Journal-region version 3 additionally supports bounded multi-transaction retention through two alternating snapshot banks. The inactive bank is fully written and flushed before the first-block anchor flips to a higher generation, so write-through crash testing can expose only the previous complete retained log or the replacement complete retained log. Checkpoint continues to collapse either state to the canonical v2 empty anchor. This is not yet circular head/tail wraparound.

Retained v3 snapshots can now advance a filesystem-consistent complete-transaction prefix without discarding the suffix. Prefix home writes are semantically preflighted through strict fsck, replayed and flushed, then the suffix is atomically republished through the inactive bank. This reclaims bounded retained capacity and provides persistent logical head advancement while still deliberately stopping short of physical circular head/tail wraparound.

The directory-table transaction layer is the first production metadata consumer of retained mode. Each retained namespace update loads directory state through the current WAL recovery projection, so dependent updates compose against logical state rather than stale home blocks. The full candidate retained stream is strict-fsck projected before append; invalid namespace candidates therefore fail before journal mutation. Other transaction families still use their existing immediate replay/checkpoint contracts.

## Format-v6 byte EOF milestone

Filesystem format v6 promotes exact regular-file EOF into inode-record version 3. Whole-file and
range reads stop at that persisted EOF, overwrite-only byte writes cannot extend it, and exact-byte
truncate can shrink into a partial final block while zeroing discarded tail bytes and releasing only
the trailing block suffix. Exact-byte growth may remain inside the current final block or allocate the
minimum required trailing blocks; every newly visible byte is zero. Extending byte writes build on the
same non-sparse model but combine payload publication with growth in one WAL transaction, so a crash
cannot expose a zero-grown file without the write payload. Grow, shrink, and extending write publish
their data/allocator/inode changes atomically, and deterministic crash tests require recovery to
converge to the complete old or complete new state.

Whole-file clone, exchange, and transfer-replace carry exact EOF together with file contents, so a
partial-final-block file remains semantically whole across those operations. The filesystem now
supports non-sparse exact-byte growth but still does not claim sparse holes or general extent
semantics.

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

The checkpoint still deliberately does not define:

- a circular journal with persistent head/tail and wraparound reuse beyond the bounded dual-bank retained-snapshot reservation;
- sparse files, hole punching, or a general extent data model;
- persisted hard-link counts;
- permissions, ACLs, mmap, FUSE, or broad POSIX compatibility;
- stronger hardware fault models such as torn sectors or storage reordering.

Those should be reopened only as separately specified, bounded milestones with their own invariants and crash model. Routine maintenance after this checkpoint should otherwise be patrol-driven: regressions, corruption acceptance, recovery/fsck disagreement, or another concrete correctness defect justify changes; repository activity by itself does not.
