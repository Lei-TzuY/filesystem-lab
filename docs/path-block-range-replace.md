# Pathname variable-length block-range replacement

`replace_file_blocks_at_path_journaled` atomically replaces a non-empty existing logical-block range of one regular file with a non-empty caller-provided sequence of complete blocks. The replacement length may differ from the displaced range length, so the inode block vector may grow or shrink.

Before pathname resolution, the entry point recovers and checkpoints any older committed WAL. Path resolution therefore observes recovered namespace state rather than a partially replayed home image, then follows intermediate and final symbolic links using the repository-wide bounded resolver. The resolved inode is delegated to `replace_file_blocks_journaled`.

The durable primitive allocates every fresh replacement block before releasing any displaced block, then publishes allocation metadata, the resized inode table image, and all replacement data-block images through one WAL transaction. Exactly the displaced physical blocks become free; surviving file blocks remain owned and retain their data. Namespace metadata is not modified by the replacement transaction itself.

The deterministic replacement crash matrix enumerates every injected write/flush interruption. After recovery, allocator ownership, inode references, and visible replacement data must correspond to either the complete old state or the complete new state. A dedicated recovery-boundary regression also interrupts creation of a final symlink after durable commit but before complete home replay, then invokes pathname replacement without an external recovery step. The pathname entry point must recover that older namespace WAL before resolving the alias, after which replacement data, released-block ownership, allocator/inode accounting, unique physical references, fsck cleanliness, journal checkpointing, and second-recovery idempotence are revalidated.

Filesystem format remains v5. Because v5 persists no byte length, this API is intentionally full-block and existing-range only. It does not define byte-range replacement, EOF, sparse holes, extents, reflinks, or POSIX splice semantics.
