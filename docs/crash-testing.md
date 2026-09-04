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

`tests/create_crash_matrix.rs` applies the same enumeration to the three-table atomic create path, including the successful-path journal checkpoint. A crash may occur while publishing the journal, installing allocation/inode/directory home blocks, clearing the fixed journal reservation, or crossing the checkpoint flush.

Before recovery, a durable image is accepted only when it is the complete old state or complete new state; any partially installed combination must be rejected by fsck. A durable commit must replay all three home writes. `recover_journal_and_checkpoint` then makes the repaired home state durable before clearing the journal, and the recovered reservation must be empty. A second recover-and-checkpoint pass is a no-op. The success-path regression also performs a second create immediately after the first, proving that checkpoint completion makes the same fixed journal reservation reusable without an intervening mount-style recovery pass.

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

## Single-block truncate matrix

`tests/truncate_last_block_crash_matrix.rs` exercises the shrink-side counterpart of block append. The operation removes exactly the final physical block reference from one regular-file inode and releases exactly that block from the allocator in the same WAL transaction; the inode identity and namespace remain unchanged.

The test enumerates every journal publication, allocation/inode home-write, home flush, journal-clear, and checkpoint-flush boundary. After reboot, a directly fsck-valid image must be either the complete old two-block file or the complete new one-block file. Any partial allocator/inode combination must be rejected by fsck. `recover_journal_and_checkpoint` must converge a durable commit, leave the fixed journal reservation empty, and a second recovery/checkpoint pass must be a no-op.

## Multi-block tail truncate matrix

`tests/truncate_to_blocks_crash_matrix.rs` extends the same contract to shrinking a regular file to an exact count of complete logical blocks. All trailing inode references and all corresponding allocator ownership bits are advanced in one WAL transaction; inode identity and namespace are unchanged.

The regression truncates a four-block file to one block and enumerates every deterministic mutation boundary through journal publication, allocation/inode replay, home durability, journal clearing, and checkpoint durability. A reboot-visible state accepted by fsck must be either the complete four-block old state or the complete one-block new state. Any mixed allocator/inode tail must be rejected before recovery. A durable commit must converge to the complete new state, the fixed journal reservation must end empty, and a second recovery/checkpoint pass must be a no-op.

These truncate contracts remain block-granular because format v5 does not persist byte length. Partial-block truncation, sparse holes, and byte-stream POSIX truncate semantics remain separate format and lifecycle decisions.

Future lifecycle operations can reuse the same `CrashDevice` and enumeration pattern so later multi-block namespace and file-data transitions are checked against every write/flush boundary rather than isolated injected failures.
