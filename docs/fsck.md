# Fsck consistency contract

`filesystem-lab` exposes a read-only consistency checker through `fsck::check_device`. Its scope deliberately matches the durable layers that exist today: the version-3 superblock, persistent allocation image, and bounded persistent journal region. Inode and directory state remain in-memory only, so fsck does not pretend to validate durable metadata that has not been defined yet.

## Checks performed

The checker validates, in order:

1. block zero decodes as a valid version-3 superblock;
2. the superblock block count matches the opened device;
3. journal and allocation reservations are contiguous, non-empty, inside the device, and form one reserved metadata prefix;
4. the allocation image has valid magic/version/flags/reserved fields, exact bitmap length, zero padding, CRC-32, zero reserved-metadata bits, and zero trailing bits;
5. the reconstructed allocator satisfies `allocated + free = data_blocks` and its in-memory executable accounting invariant;
6. the complete journal-region image has valid magic/version/flags/reserved bytes, length, zero padding, and CRC-32;
7. every journal record decodes with its own framing/version/checksum constraints;
8. transaction ordering is structurally valid;
9. every journal write targets either an ordinary data home block or a block in the allocation-metadata home region. Superblock and journal-reservation targets remain forbidden.

The result reports filesystem geometry, allocated/free block counts, journal entry/write counts, committed transaction count, and an optional pending transaction identifier.

## Crash semantics

An incomplete final journal transaction is **not** corruption. It is a valid durable prefix representing a crash before the commit marker reached stable storage. The checker reports it as `pending_transaction`; recovery continues to ignore its writes.

Allocation images still use tail-block-first, header-block-last ordering when the direct persistence primitive is used. The journaled allocator path instead records the complete allocation image in one committed WAL transaction, flushes the journal, replays those allocation home blocks, and then flushes home locations. A crash before commit leaves the old allocation image untouched; a failure after commit remains recoverable by idempotent replay of the durable journal.

`check_device` is intentionally read-only: it performs no block writes and crosses no flush boundary. Repair/checkpoint policy remains a later milestone.

## Future extension

When inode and directory metadata gain explicit persistent formats, fsck should extend this same report with executable cross-layer invariants such as no double ownership across inode references, inode-to-block reference validity, and namespace reachability. Those checks must follow explicit on-disk format revisions rather than inferring persistence from current in-memory models.
