# Pathname multi-block regular-file create

`create_file_with_blocks_at_path_journaled` extends format-v5 pathname creation from an empty or one-block regular file to a bounded file containing multiple fully initialized logical blocks.

## Contract

The API accepts an absolute destination pathname and a non-empty slice of complete 4 KiB logical-block images. It deliberately does not accept a byte length: format v5 still has no persisted EOF field, so each supplied image represents one complete logical block.

Before resolving the destination parent or loading mutable filesystem metadata, the operation recovers and checkpoints any older durable WAL. Parent lookup therefore derives its inode from recovered namespace state even after an earlier committed transaction crashed during home replay.

After destination validation, first-fit allocation chooses one distinct physical data block for every logical input block. One fresh regular-file inode references those blocks in input order, and one namespace entry links the destination name to that inode.

Allocation metadata, the inode table, the directory table, and every changed initial data block are logged in one WAL transaction. Journal capacity is a hard bound: the operation fails before publishing a replacement journal image when the complete transaction cannot fit.

## Crash semantics

Deterministic `write_block`/`flush` crash enumeration verifies every mutation point of a successful three-block create. After reboot and recovery, exactly one of two states is allowed:

- pre-commit: allocator, inode table, and namespace remain exactly at the old state and the destination does not exist;
- post-commit: the destination exists as one regular-file inode with every initial logical block present in order and containing its complete requested image.

A partially initialized or prefix-length file is never accepted as a committed result. Every recovered state must also satisfy allocator accounting, unique physical block ownership, allocator ownership for every inode reference, read-only fsck, an empty checkpointed journal, and idempotent second recovery.

## Format compatibility

This capability does **not** change the on-disk format. The filesystem remains format v5, with the existing allocation image, inode block-reference vector, directory table, WAL record format, and full-block data representation. No migration is required.
