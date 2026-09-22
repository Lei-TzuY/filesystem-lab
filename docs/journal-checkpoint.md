# Journal checkpoint durability contract

Filesystem format v5 still uses one bounded fixed journal reservation. The journal-region codec is
versioned independently; writers now publish **journal region v2** anchors while the reader retains
compatibility with complete version-1 images.

A checkpoint is allowed only after recovery has made every committed home write durable. It then
publishes one checksummed v2 **empty anchor** in the first journal block and flushes that anchor.
Later journal blocks are deliberately left untouched: while the empty anchor is authoritative, stale
tail bytes do not describe an active log.

This ordering matches the actual `BlockDevice` contract. A successful `write_block` is allowed to
reach durable storage before the next `flush`; `flush` guarantees that all prior writes are durable,
but does not promise they were volatile beforehand. Under the repository's whole-block persistence
model, a crash around checkpoint therefore exposes either:

- the previous complete active journal, which can be replayed again; or
- the complete empty anchor, after home replay was already flushed.

It cannot expose a partially zeroed multi-block journal merely because some pre-flush writes reached
storage early.

`recover_journal_and_checkpoint` composes replay and checkpoint in this order:

1. load and validate the persistent journal;
2. replay committed records to home locations;
3. flush replayed home writes;
4. publish the v2 empty anchor in the first journal block;
5. flush the empty anchor.

Journal publication uses the complementary ordering: establish a durable empty anchor, stage every
non-anchor journal block, flush those tail blocks, then publish and flush the active anchor. Thus an
active anchor is never authoritative before all bytes it references are durable.

Deterministic crash matrices cover both the original flush-controlled model and a write-through model
where every successful whole-block write becomes durable immediately. After reboot, journal state
must always decode as either empty or a complete active image; recovery remains idempotent.

Sector tearing, controller reordering, and partial-block persistence remain outside the fault model.
