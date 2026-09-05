# Regular-file logical block insertion

Format v5 exposes `insert_file_block_journaled` as a bounded block-granular growth primitive. It allocates exactly one physical block and inserts that reference at a caller-selected logical index from zero through the current block count, shifting the existing logical suffix right.

The allocator image, inode table image, and complete new data block are published in one WAL transaction. Namespace metadata is unchanged. A crash before durable commit preserves the old ownership/reference state; after commit, recovery converges to the complete inserted state even when only a prefix of home writes reached durable storage.

Deterministic crash enumeration requires allocator/inode agreement, no double ownership, fsck cleanliness after recovery, an empty checkpointed journal, and idempotent second recovery. Mixed ownership/reference home states are not accepted merely because individual metadata regions decode.

This operation does not change the format-v5 schema and does not claim persisted byte length, EOF, sparse holes, byte-level insertion, or extent semantics.
