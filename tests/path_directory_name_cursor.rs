use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::path_directory::{list_directory_page_after_name_at_path, PathDirectoryEntry};
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
            inode(5, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[
            entry(1, 5, "zeta"),
            entry(2, 3, "beta"),
            entry(1, 2, "dir"),
            entry(2, 4, "delta"),
            entry(2, 5, "gamma"),
            entry(2, 3, "alpha"),
        ],
    )
    .unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "dir-link", "dir").unwrap();
    (device, superblock)
}

#[test]
fn starts_after_exact_name_and_honors_limit() {
    let (mut device, superblock) = setup();

    assert_eq!(
        list_directory_page_after_name_at_path(&mut device, &superblock, "/dir", Some("beta"), 2,)
            .unwrap(),
        vec![
            PathDirectoryEntry {
                name: "delta".to_owned(),
                inode_id: 4,
                kind: InodeKind::Directory,
            },
            PathDirectoryEntry {
                name: "gamma".to_owned(),
                inode_id: 5,
                kind: InodeKind::File,
            },
        ]
    );
}

#[test]
fn nonexistent_cursor_uses_lexical_insertion_boundary() {
    let (mut device, superblock) = setup();

    assert_eq!(
        list_directory_page_after_name_at_path(
            &mut device,
            &superblock,
            "/dir",
            Some("charlie"),
            8,
        )
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>(),
        vec!["delta".to_owned(), "gamma".to_owned()]
    );
}

#[test]
fn none_starts_from_first_entry_and_zero_limit_is_empty() {
    let (mut device, superblock) = setup();

    assert_eq!(
        list_directory_page_after_name_at_path(&mut device, &superblock, "/dir", None, 2)
            .unwrap()
            .into_iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>(),
        vec!["alpha".to_owned(), "beta".to_owned()]
    );
    assert!(list_directory_page_after_name_at_path(
        &mut device,
        &superblock,
        "/dir",
        Some("alpha"),
        0,
    )
    .unwrap()
    .is_empty());
}

#[test]
fn cursor_past_end_is_empty_and_final_symlink_is_followed() {
    let (mut device, superblock) = setup();

    assert!(list_directory_page_after_name_at_path(
        &mut device,
        &superblock,
        "/dir-link",
        Some("zzzz"),
        4,
    )
    .unwrap()
    .is_empty());
    assert_eq!(
        list_directory_page_after_name_at_path(
            &mut device,
            &superblock,
            "/dir-link",
            Some("delta"),
            1,
        )
        .unwrap()[0]
            .name,
        "gamma"
    );
}
