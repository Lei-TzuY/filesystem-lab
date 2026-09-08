# Pathname one-block file creation

Format v5 exposes bounded pathname creation of a regular file with exactly one initialized logical block through `create_one_block_file_at_path_journaled`.

The destination parent follows the existing bounded symbolic-link traversal rules. The final component is not followed and must not already exist. Creation allocates one physical data block and publishes the allocator image, new regular-file inode, namespace entry, and complete initial 4 KiB data image in one WAL transaction. A successful return includes journal recovery and checkpointing, so the fixed journal reservation is immediately reusable.

Crash/fault enumeration covers modeled write and flush interruption points. Recovery must expose either the complete old state or the complete new file; it must never expose namespace/inode ownership without the committed initial data image. Post-recovery validation requires exact allocator ownership, no duplicate physical ownership, valid inode references, reachable namespace, clean read-only fsck, an empty checkpointed journal, and idempotent second recovery.

This does not change the on-disk format. Format v5 has no persisted byte EOF, so the operation deliberately creates one complete logical block rather than claiming partial-block file length, sparse holes, extent semantics, permissions, timestamps, or general POSIX `open(O_CREAT)` behavior.
