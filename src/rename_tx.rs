use std::collections::{BTreeMap, BTreeSet};
use std::io;

use crate::block::BlockDevice;
use crate::directory_codec::{encode_directory_entry, PersistedDirectoryEntry};
use crate::directory_table::load_directory_table;
use crate::directory_tx::store_directory_table_journaled;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::recovery::RecoveryReport;

/// Atomically renames one durable namespace entry through the directory-table WAL.
///
/// This deliberately implements only a narrow rename transition: the source entry must already
/// exist, the destination name must not exist, the target inode is unchanged, and no inode or
/// allocation metadata is modified. Moving the root inode and moving a directory underneath one of
/// its descendants are rejected before a new journal image is published. Overwrite/exchange rename
/// semantics remain out of scope.
///
/// # Errors
///
/// Returns `InvalidInput` when either parent is missing or is not a directory, the source is
/// missing, the destination already exists, the root inode would be moved, the new name is invalid,
/// or the move would introduce a directory cycle. Durable metadata read, journal, recovery, and
/// device failures propagate.
pub fn rename_entry_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    old_parent: u64,
    old_name: &str,
    new_parent: u64,
    new_name: &str,
) -> io::Result<RecoveryReport> {
    let inodes = load_inode_table(device, superblock)?;
    validate_directory_parent(&inodes, old_parent, "rename source parent")?;
    validate_directory_parent(&inodes, new_parent, "rename destination parent")?;

    let mut entries = load_directory_table(device, superblock)?;
    let source_index = entries
        .iter()
        .position(|entry| entry.parent == old_parent && entry.name == old_name)
        .ok_or_else(|| invalid_input("rename source entry does not exist"))?;
    let target = entries[source_index].target;

    if target == 1 {
        return Err(invalid_input("root inode cannot be renamed"));
    }

    if old_parent == new_parent && old_name == new_name {
        return Ok(RecoveryReport::default());
    }

    if entries.iter().enumerate().any(|(index, entry)| {
        index != source_index && entry.parent == new_parent && entry.name == new_name
    }) {
        return Err(invalid_input("rename destination already exists"));
    }

    let candidate = PersistedDirectoryEntry {
        parent: new_parent,
        target,
        name: new_name.to_owned(),
    };
    encode_directory_entry(&candidate)?;

    let target_inode = inodes
        .iter()
        .find(|inode| inode.id == target)
        .ok_or_else(|| invalid_input("rename source targets a missing inode"))?;
    if target_inode.kind == InodeKind::Directory
        && would_create_directory_cycle(&entries, source_index, target, new_parent)
    {
        return Err(invalid_input("rename would create a directory cycle"));
    }

    entries[source_index] = candidate;
    store_directory_table_journaled(device, superblock, &entries)
}

fn validate_directory_parent(
    inodes: &[crate::inode_codec::PersistedInode],
    parent: u64,
    label: &str,
) -> io::Result<()> {
    let inode = inodes
        .iter()
        .find(|inode| inode.id == parent)
        .ok_or_else(|| invalid_input(format!("{label} inode does not exist")))?;
    if inode.kind != InodeKind::Directory {
        return Err(invalid_input(format!("{label} inode is not a directory")));
    }
    Ok(())
}

fn would_create_directory_cycle(
    entries: &[PersistedDirectoryEntry],
    source_index: usize,
    target: u64,
    new_parent: u64,
) -> bool {
    if target == new_parent {
        return true;
    }

    let mut children: BTreeMap<u64, Vec<u64>> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        if index != source_index {
            children
                .entry(entry.parent)
                .or_default()
                .push(entry.target);
        }
    }

    let mut pending = vec![target];
    let mut seen = BTreeSet::new();
    while let Some(inode) = pending.pop() {
        if !seen.insert(inode) {
            continue;
        }
        if inode == new_parent {
            return true;
        }
        if let Some(next) = children.get(&inode) {
            pending.extend(next.iter().copied());
        }
    }
    false
}

fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::BLOCK_SIZE;
    use crate::directory_table::{load_directory_table, store_directory_table};
    use crate::format::format_device;
    use crate::inode_codec::PersistedInode;
    use crate::inode_table::store_inode_table;
    use crate::recovery::recover_journal;

    #[derive(Debug)]
    struct FaultDevice {
        blocks: Vec<[u8; BLOCK_SIZE]>,
        fail_once_on: Option<u64>,
    }

    impl FaultDevice {
        fn new(blocks: usize) -> Self {
            Self {
                blocks: vec![[0_u8; BLOCK_SIZE]; blocks],
                fail_once_on: None,
            }
        }
    }

    impl BlockDevice for FaultDevice {
        fn block_count(&self) -> u64 {
            u64::try_from(self.blocks.len()).expect("test device length fits u64")
        }

        fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
            let index = usize::try_from(block)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
            *buf = *self
                .blocks
                .get(index)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))?;
            Ok(())
        }

        fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
            if self.fail_once_on == Some(block) {
                self.fail_once_on = None;
                return Err(io::Error::other("injected rename home write failure"));
            }
            let index = usize::try_from(block)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "block exceeds usize"))?;
            *self
                .blocks
                .get_mut(index)
                .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))? = *buf;
            Ok(())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn inode(id: u64, kind: InodeKind) -> PersistedInode {
        PersistedInode {
            id,
            kind,
            blocks: Vec::new(),
        }
    }

    fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
        PersistedDirectoryEntry {
            parent,
            target,
            name: name.to_owned(),
        }
    }

    fn setup(entries: &[PersistedDirectoryEntry]) -> (FaultDevice, Superblock) {
        let mut device = FaultDevice::new(64);
        let superblock = format_device(&mut device).unwrap();
        store_inode_table(
            &mut device,
            &superblock,
            &[
                inode(1, InodeKind::Directory),
                inode(2, InodeKind::Directory),
                inode(3, InodeKind::Directory),
                inode(4, InodeKind::File),
            ],
        )
        .unwrap();
        store_directory_table(&mut device, &superblock, entries).unwrap();
        (device, superblock)
    }

    #[test]
    fn rename_replaces_exactly_one_namespace_key() {
        let original = [entry(1, 4, "old")];
        let (mut device, superblock) = setup(&original);

        let report =
            rename_entry_journaled(&mut device, &superblock, 1, "old", 2, "new").unwrap();

        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 1);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            vec![entry(2, 4, "new")]
        );
    }

    #[test]
    fn committed_rename_recovers_after_home_write_failure() {
        let original = [entry(1, 4, "old")];
        let (mut device, superblock) = setup(&original);
        device.fail_once_on = Some(superblock.directory_start);

        assert_eq!(
            rename_entry_journaled(&mut device, &superblock, 1, "old", 2, "new")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            original
        );

        let report = recover_journal(&mut device, superblock).unwrap();
        assert_eq!(report.committed_transactions, 1);
        assert_eq!(report.home_writes, 1);
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            vec![entry(2, 4, "new")]
        );

        assert_eq!(recover_journal(&mut device, superblock).unwrap(), report);
    }

    #[test]
    fn rename_rejects_destination_overwrite_before_wal() {
        let original = [entry(1, 4, "old"), entry(2, 3, "new")];
        let (mut device, superblock) = setup(&original);

        assert_eq!(
            rename_entry_journaled(&mut device, &superblock, 1, "old", 2, "new")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            original
        );
    }

    #[test]
    fn rename_rejects_directory_move_into_descendant() {
        let original = [entry(1, 2, "a"), entry(2, 3, "b")];
        let (mut device, superblock) = setup(&original);

        assert_eq!(
            rename_entry_journaled(&mut device, &superblock, 1, "a", 3, "a")
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            load_directory_table(&mut device, &superblock).unwrap(),
            original
        );
    }
}
