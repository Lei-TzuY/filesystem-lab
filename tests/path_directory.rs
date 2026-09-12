use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::path_directory::{
    list_directory_at_path, list_directory_page_at_path, PathDirectoryEntry,
};
use filesystem_lab::symlink::create_symlink_journaled;

const JOURNAL_BLOCKS: u64 = 8;

struct MemoryDevice {
    blocks: Vec<[u8; BLOCK_SIZE]>,
}

impl MemoryDevice {
    fn new(blocks: usize) -> Self {
        Self {
            blocks: vec![[0; BLOCK_SIZE]; blocks],
        }
    }

    fn block_index(&self, block: u64) -> io::Result<usize> {
        usize::try_from(block)
            .ok()
            .filter(|index| *index < self.blocks.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "invalid block"))
    }
}

impl BlockDevice for MemoryDevice {
    fn block_count(&self) -> u64 {
        u64::try_from(self.blocks.len()).expect("test device block count fits in u64")
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = self.block_index(block)?;
        *buf = self.blocks[index];
        Ok(())
    }

    fn write_block(&mut self, block: u64, buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        let index = self.block_index(block)?;
        self.blocks[index] = *buf;
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

fn setup() -> (MemoryDevice, Superblock) {
    let mut device = MemoryDevice::new(96);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::File),
            inode(4, InodeKind::Directory),
            inode(5, InodeKind::Directory),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 3, "file"),
            entry(2, 4, "nested"),
            entry(1, 5, "empty"),
            entry(1, 2, "dir"),
            entry(2, 3, "alpha"),
        ],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir-link", "dir").unwrap();
    (device, superblock)
}

#[test]
fn lists_root_children_deterministically_without_following_child_symlinks() {
    let (mut device, superblock) = setup();

    assert_eq!(
        list_directory_at_path(&mut device, &superblock, "/").unwrap(),
        vec![
            PathDirectoryEntry {
                name: "dir".to_owned(),
                inode_id: 2,
                kind: InodeKind::Directory,
            },
            PathDirectoryEntry {
                name: "dir-link".to_owned(),
                inode_id: 6,
                kind: InodeKind::Symlink,
            },
            PathDirectoryEntry {
                name: "empty".to_owned(),
                inode_id: 5,
                kind: InodeKind::Directory,
            },
            PathDirectoryEntry {
                name: "file".to_owned(),
                inode_id: 3,
                kind: InodeKind::File,
            },
        ]
    );
}

#[test]
fn paginates_the_deterministic_directory_snapshot() {
    let (mut device, superblock) = setup();

    assert_eq!(
        list_directory_page_at_path(&mut device, &superblock, "/", 1, 2).unwrap(),
        vec![
            PathDirectoryEntry {
                name: "dir-link".to_owned(),
                inode_id: 6,
                kind: InodeKind::Symlink,
            },
            PathDirectoryEntry {
                name: "empty".to_owned(),
                inode_id: 5,
                kind: InodeKind::Directory,
            },
        ]
    );
    assert!(
        list_directory_page_at_path(&mut device, &superblock, "/", 0, 0)
            .unwrap()
            .is_empty()
    );
    assert!(
        list_directory_page_at_path(&mut device, &superblock, "/", 4, 3)
            .unwrap()
            .is_empty()
    );
    assert!(
        list_directory_page_at_path(&mut device, &superblock, "/", usize::MAX, 1)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn resolves_nested_and_final_symlink_directory_paths() {
    let (mut device, superblock) = setup();
    let expected = vec![
        PathDirectoryEntry {
            name: "alpha".to_owned(),
            inode_id: 3,
            kind: InodeKind::File,
        },
        PathDirectoryEntry {
            name: "nested".to_owned(),
            inode_id: 4,
            kind: InodeKind::Directory,
        },
    ];

    assert_eq!(
        list_directory_at_path(&mut device, &superblock, "/dir").unwrap(),
        expected
    );
    assert_eq!(
        list_directory_at_path(&mut device, &superblock, "/dir/").unwrap(),
        expected
    );
    assert_eq!(
        list_directory_at_path(&mut device, &superblock, "/dir-link").unwrap(),
        expected
    );
    assert_eq!(
        list_directory_at_path(&mut device, &superblock, "/./dir").unwrap(),
        expected
    );
    assert_eq!(
        list_directory_at_path(&mut device, &superblock, "/dir/nested/..").unwrap(),
        expected
    );
}

#[test]
fn empty_directory_returns_an_empty_listing() {
    let (mut device, superblock) = setup();
    assert!(list_directory_at_path(&mut device, &superblock, "/empty")
        .unwrap()
        .is_empty());
}

#[test]
fn rejects_non_directory_and_preserves_bounded_path_validation() {
    let (mut device, superblock) = setup();

    let error = list_directory_at_path(&mut device, &superblock, "/file").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    for path in ["dir", "/dir//", "/dir//nested", "/missing"] {
        assert!(list_directory_at_path(&mut device, &superblock, path).is_err());
    }
}

#[test]
fn rejects_durable_namespace_target_missing_from_inode_table() {
    let (mut device, superblock) = setup();
    store_directory_table(&mut device, &superblock, &[entry(1, 999, "dangling-entry")]).unwrap();

    let error = list_directory_at_path(&mut device, &superblock, "/").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("missing inode"));
}
