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

Future lifecycle operations can reuse the same `CrashDevice` and enumeration pattern so create, unlink, truncate, and later multi-block namespace transitions are checked against every write/flush boundary rather than isolated injected failures.
