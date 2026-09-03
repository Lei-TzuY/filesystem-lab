# Deterministic crash testing

Crash-consistency tests should enumerate persistence mutation boundaries instead of covering only one hand-picked device failure. The integration-test `CrashDevice` in `tests/support` models two block-device views:

- **volatile** blocks receive ordinary `write_block` calls and are visible to reads in the running instance;
- **durable** blocks advance only when `flush` succeeds.

Once armed, every `write_block` and `flush` is assigned a deterministic mutation-operation index. Injecting a crash at index `N` fails before that operation takes effect. `reboot()` discards the volatile view and restores the last successfully flushed durable image, then disables fault injection so recovery can run on the simulated post-crash device.

This model is intentionally small. It does not emulate sector tearing, controller reordering, or partial-block writes; those remain separate fault models. Its purpose is to make the filesystem's declared write/flush ordering executable and exhaustive for a bounded operation.

## Rename matrix

`tests/rename_crash_matrix.rs` first measures the mutation-operation count of one successful bounded rename. It then re-creates the same valid filesystem state and injects a crash before every mutation operation in turn.

For every crash point, the regression requires:

1. after reboot, the durable directory table is still the complete old namespace rather than a half-applied rename;
2. read-only fsck accepts that durable pre-recovery namespace;
3. if the journal commit was not durable, recovery is a no-op and the old namespace remains;
4. if the journal commit was durable, recovery installs the complete new namespace in one replayed transaction;
5. fsck accepts the recovered namespace;
6. a second recovery is idempotent and does not change the recovered state.

The matrix therefore turns the rename durability contract into an executable invariant: every bounded crash point resolves to either the old namespace or a committed journal that deterministically recovers the new namespace. It does not add overwrite/exchange rename semantics or alter filesystem format v5.

## Create matrix

`tests/create_crash_matrix.rs` applies the same enumeration to the three-table atomic create path. A crash may occur while publishing the journal or while installing allocation, inode, and directory home blocks. Before recovery, a durable image is accepted only when it is the complete old state or complete new state; any partially installed combination must be rejected by fsck. A durable commit must replay all three home writes, and replay must remain idempotent.

## Unlink matrix

`tests/unlink_crash_matrix.rs` starts from a directly seeded, fsck-clean linked file so the journal is empty before the unlink transaction begins. It then measures one successful validated unlink and injects a crash before every `write_block` or `flush` in that operation.

For every crash point, the regression requires:

1. a pre-commit crash reboots to the complete linked-file state and recovery remains a no-op;
2. a fully installed unlink is accepted directly by fsck;
3. any partially installed allocation/inode/directory home state is rejected by fsck before recovery;
4. a durable unlink commit replays exactly the three changed home blocks and converges to the complete unlinked state;
5. the recovered state has no namespace entry, no removed inode, and no ownership of the removed inode's data block;
6. a second recovery returns the same report and leaves the state unchanged.

Together, the create and unlink matrices exercise opposite directions of the same cross-layer ownership invariant: namespace publication must agree with inode existence and data-block ownership at every durable boundary or be repairable from one committed WAL transaction.

Future lifecycle operations can reuse the same `CrashDevice` and enumeration pattern so truncate and later multi-block namespace transitions are checked against every write/flush boundary rather than isolated injected failures.
