# Fsck consistency contract

`filesystem-lab` currently exposes a read-only consistency checker through `fsck::check_device`.
Its scope deliberately matches the durable layers that exist today: the version-2 superblock and the
bounded persistent journal region. Allocation, inode, and directory state are still in-memory only,
so fsck does not pretend to validate durable metadata that has not been defined yet.

## Checks performed

The checker validates, in order:

1. block zero decodes as a valid version-2 superblock;
2. the superblock block count matches the opened device;
3. the journal reservation is contiguous, non-empty, inside the device, and part of the reserved
   metadata prefix;
4. the complete journal-region image has valid magic/version/flags/reserved bytes, length, zero
   padding, and CRC-32;
5. every journal record decodes with its own framing/version/checksum constraints;
6. transaction ordering is structurally valid;
7. every journal write targets a home block in the data region, never block zero or the journal
   reservation.

The result reports filesystem geometry, journal entry/write counts, committed transaction count,
and an optional pending transaction identifier.

## Crash semantics

An incomplete final transaction is **not** corruption. It is a valid durable prefix representing a
crash before the commit marker reached stable storage. The checker reports it as
`pending_transaction`; recovery will continue to ignore its writes.

Any malformed non-zero journal image is rejected rather than guessed, truncated, or repaired.
`check_device` is intentionally read-only: it performs no block writes and crosses no flush boundary.
Repair/checkpoint policy remains a later milestone.

## Future extension

When allocation, inode, and directory metadata gain explicit persistent formats, fsck should extend
this same report with executable cross-layer invariants such as allocated/free accounting, no double
ownership, inode-to-block reference validity, and namespace reachability. Those checks must follow
an explicit on-disk format revision rather than inferring persistence from the current in-memory
models.
