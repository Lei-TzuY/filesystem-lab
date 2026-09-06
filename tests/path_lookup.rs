use std::io;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::path_lookup::{resolve_path_following_symlinks, MAX_SYMLINK_EXPANSIONS};
use filesystem_lab::symlink::create_symlink_journaled;

const SYMLINK_JOURNAL_BLOCKS: u64 = 6;

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
    let mut device = MemoryDevice::new(64);
    let superblock =
        format_device_with_journal_blocks(&mut device, SYMLINK_JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory),
            inode(2, InodeKind::Directory),
            inode(3, InodeKind::File),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "dir"), entry(2, 3, "file")],
    )
    .unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn resolves_root_and_ordinary_absolute_paths() {
    let (mut device, superblock) = setup();

    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/").unwrap(),
        1
    );
    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/dir/file").unwrap(),
        3
    );
}

#[test]
fn follows_relative_and_absolute_symlink_targets_with_suffixes() {
    let (mut device, superblock) = setup();
    create_symlink_journaled(&mut device, &superblock, 1, "rel", "dir/file").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "absdir", "/dir").unwrap();
    create_symlink_journaled(&mut device, &superblock, 2, "local", "file").unwrap();

    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/rel").unwrap(),
        3
    );
    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/absdir/file").unwrap(),
        3
    );
    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/dir/local").unwrap(),
        3
    );
    check_device(&mut device).unwrap();
}

#[test]
fn rejects_dangling_links_and_symlink_loops() {
    let (mut device, superblock) = setup();
    create_symlink_journaled(&mut device, &superblock, 1, "dangling", "missing").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "a", "/b").unwrap();
    create_symlink_journaled(&mut device, &superblock, 1, "b", "/a").unwrap();

    assert_eq!(
        resolve_path_following_symlinks(&mut device, &superblock, "/dangling")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    let error = resolve_path_following_symlinks(&mut device, &superblock, "/a").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("expansion limit"));
    assert_eq!(MAX_SYMLINK_EXPANSIONS, 40);
}

#[test]
fn rejects_ambiguous_or_non_absolute_paths() {
    let (mut device, superblock) = setup();

    for path in [
        "dir/file",
        "/dir/./file",
        "/dir/../file",
        "/dir//file",
        "/dir/",
    ] {
        assert_eq!(
            resolve_path_following_symlinks(&mut device, &superblock, path)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidInput,
            "path {path:?} must be rejected"
        );
    }
}
