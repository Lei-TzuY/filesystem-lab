use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_data::append_file_block_journaled;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::hard_link_tx::hard_link_file_journaled;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::path_metadata::{metadata_at_path, symlink_metadata_at_path, PathMetadata};
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

fn setup() -> (MemoryDevice, Superblock, u64, u64) {
    let mut device = MemoryDevice::new(80);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[inode(1, InodeKind::Directory), inode(2, InodeKind::File)],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "file".to_owned(),
        }],
    )
    .unwrap();

    append_file_block_journaled(&mut device, &superblock, 2, [0x5a; BLOCK_SIZE]).unwrap();
    hard_link_file_journaled(&mut device, &superblock, 1, "alias", 2).unwrap();
    let (link_inode, _) =
        create_symlink_journaled(&mut device, &superblock, 1, "link", "file").unwrap();
    let (dangling_inode, _) =
        create_symlink_journaled(&mut device, &superblock, 1, "dangling", "missing").unwrap();
    (device, superblock, link_inode, dangling_inode)
}

#[test]
fn reports_root_and_regular_file_metadata_without_inventing_byte_length() {
    let (mut device, superblock, _, _) = setup();

    assert_eq!(
        metadata_at_path(&mut device, &superblock, "/").unwrap(),
        PathMetadata {
            inode_id: 1,
            kind: InodeKind::Directory,
            logical_blocks: 0,
            namespace_references: 0,
        }
    );
    assert_eq!(
        metadata_at_path(&mut device, &superblock, "/file").unwrap(),
        PathMetadata {
            inode_id: 2,
            kind: InodeKind::File,
            logical_blocks: 1,
            namespace_references: 2,
        }
    );
    assert_eq!(
        metadata_at_path(&mut device, &superblock, "/./file").unwrap(),
        metadata_at_path(&mut device, &superblock, "/file").unwrap()
    );
}

#[test]
fn hard_link_aliases_report_the_same_inode_and_derived_reference_count() {
    let (mut device, superblock, _, _) = setup();

    let primary = metadata_at_path(&mut device, &superblock, "/file").unwrap();
    let alias = metadata_at_path(&mut device, &superblock, "/alias").unwrap();
    assert_eq!(primary, alias);
    assert_eq!(alias.namespace_references, 2);
}

#[test]
fn stat_follows_final_symlink_while_lstat_reports_the_symlink_inode() {
    let (mut device, superblock, link_inode, _) = setup();

    let followed = metadata_at_path(&mut device, &superblock, "/link").unwrap();
    assert_eq!(followed.inode_id, 2);
    assert_eq!(followed.kind, InodeKind::File);

    assert_eq!(
        symlink_metadata_at_path(&mut device, &superblock, "/link").unwrap(),
        PathMetadata {
            inode_id: link_inode,
            kind: InodeKind::Symlink,
            logical_blocks: 1,
            namespace_references: 1,
        }
    );
}

#[test]
fn lstat_can_describe_a_dangling_final_symlink() {
    let (mut device, superblock, _, dangling_inode) = setup();

    assert_eq!(
        metadata_at_path(&mut device, &superblock, "/dangling")
            .unwrap_err()
            .kind(),
        io::ErrorKind::NotFound
    );
    assert_eq!(
        symlink_metadata_at_path(&mut device, &superblock, "/dangling").unwrap(),
        PathMetadata {
            inode_id: dangling_inode,
            kind: InodeKind::Symlink,
            logical_blocks: 1,
            namespace_references: 1,
        }
    );
}

#[test]
fn rejects_allocator_ownership_disagreement_before_reporting_metadata() {
    let (mut device, superblock, _, _) = setup();
    let file_block = load_inode_table(&mut device, &superblock)
        .unwrap()
        .into_iter()
        .find(|inode| inode.id == 2)
        .unwrap()
        .blocks[0];
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    allocator.free(file_block).unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();

    let error = metadata_at_path(&mut device, &superblock, "/file").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("allocator-free"));
}

#[test]
fn preserves_existing_bounded_path_validation() {
    let (mut device, superblock, _, _) = setup();

    for path in ["file", "/file/", "/missing", "/missing//child"] {
        assert!(metadata_at_path(&mut device, &superblock, path).is_err());
        assert!(symlink_metadata_at_path(&mut device, &superblock, path).is_err());
    }
}
