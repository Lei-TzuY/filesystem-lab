# Journal checkpoint durability contract

The format-v5 journal remains a bounded fixed reservation. This milestone adds explicit checkpoint
and reuse semantics without changing the on-disk format.

A checkpoint is allowed only after recovery has made every committed home write durable. The
checkpoint validates the current journal image, overwrites the entire journal reservation with
zeroes, and performs one device `flush`. Under the laboratory `BlockDevice` durability model,
writes before a successful flush are volatile. Therefore a crash before the checkpoint flush leaves
the previous committed journal durable and replayable; a crash after the flush exposes a completely
empty journal.

This ordering is intentionally conservative. It does not claim protection against sector tearing,
controller reordering, or partial persistence of writes that have not crossed the modeled flush
boundary. Those remain outside the current crash model.

`recover_journal_and_checkpoint` composes replay and checkpoint in the required order:

1. load and validate the persistent journal;
2. replay committed records to home locations;
3. flush replayed home writes;
4. validate and zero the journal reservation;
5. flush the empty journal image.

The deterministic crash matrix enumerates every write/flush mutation boundary in that sequence. On
reboot, a committed transaction must either still be replayable from the old journal or already be
fully installed with an empty journal. A successfully checkpointed reservation can then be reused by
a later bounded transaction.
