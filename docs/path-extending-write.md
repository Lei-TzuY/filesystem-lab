# Atomic extending byte writes

Format v6 persists exact regular-file EOF. The overwrite-only byte-range API intentionally rejects
writes that cross EOF, while `file_extending_write::write_file_range_extending_journaled` provides a
separate atomic extending-write contract.

For an extending write at absolute byte offset `start` with non-empty payload:

1. the final EOF is `start + payload.len()`;
2. if `start` is beyond the old EOF, the entire gap is explicitly zero-filled;
3. only the additional trailing blocks needed by the final EOF are allocated;
4. existing visible bytes outside the payload are preserved;
5. bytes after the new partial-final-block EOF remain zero;
6. allocator ownership, inode block mapping, exact EOF, gap zeros, and payload block images are
   published in one bounded WAL transaction.

Writes wholly within EOF delegate to the existing overwrite-only range transaction. The extending
surface therefore does not weaken the original API's no-extension contract.

The implementation is deliberately non-sparse: no hole representation is created and the gap consumes
ordinary allocated blocks. Journal-capacity failure occurs before home mutation because allocator/inode
changes are rendered in memory and the complete WAL image must fit before publication.

`path_file_write::write_file_range_extending_at_path_journaled` adds checked recovery and bounded
pathname/symlink resolution before entering the inode-ID transaction.

Deterministic crash enumeration requires reboot/recovery to expose either the complete old file or the
complete new file. A committed intermediate state containing the new EOF without the payload, or the
payload without matching ownership/inode metadata, is not accepted.
