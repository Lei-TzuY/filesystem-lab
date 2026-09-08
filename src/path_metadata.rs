use std::io;

use crate::allocation_disk::load_allocator;
use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::path_lookup::{
    resolve_path_following_symlinks, resolve_path_without_following_final_symlink,
};

/// Persisted metadata that can be reported truthfully without byte-length semantics.
///
/// Format v5 does not persist a regular-file byte length, permissions, timestamps, uid/gid, or a
/// stored hard-link count. `logical_blocks` therefore reports the exact inode block-vector length,
/// while `namespace_references` is derived from the durable directory table on each query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathMetadata {
    pub inode_id: u64,
    pub kind: InodeKind,
    pub logical_blocks: usize,
    pub namespace_references: usize,
}

/// Returns metadata for an absolute pathname, following the final symbolic link.
///
/// This is the format-v5 analogue of `stat(2)`: intermediate and final symbolic links use the
/// existing bounded pathname resolver. Before metadata is returned, every physical block referenced
/// by the resolved inode is required to be a non-reserved allocator-owned data block.
///
/// # Errors
///
/// Propagates bounded pathname lookup and persisted metadata decode errors. Returns `InvalidData`
/// when the resolved inode is missing after lookup, references a reserved/free block, or a non-root
/// inode has no durable namespace reference.
pub fn metadata_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<PathMetadata> {
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    metadata_for_inode(device, superblock, inode_id)
}

/// Returns metadata for an absolute pathname without following its final symbolic link.
///
/// Intermediate symbolic links are still followed. A dangling final symlink is therefore
/// inspectable, matching `lstat(2)`-style final-component semantics without claiming unsupported
/// POSIX fields.
///
/// # Errors
///
/// Propagates bounded pathname lookup and persisted metadata decode errors. Returns `InvalidData`
/// for allocator/inode/namespace disagreement detected while describing the resolved inode.
pub fn symlink_metadata_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<PathMetadata> {
    let inode_id = resolve_path_without_following_final_symlink(device, superblock, path)?;
    metadata_for_inode(device, superblock, inode_id)
}

fn metadata_for_inode(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    inode_id: u64,
) -> io::Result<PathMetadata> {
    let allocator = load_allocator(device, superblock)?;
    let inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;
    let inode = inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "resolved inode is missing"))?;

    for block in &inode.blocks {
        if *block < superblock.reserved_blocks() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "resolved inode references a reserved metadata block",
            ));
        }
        let owned = allocator
            .is_owned(*block)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        if !owned {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "resolved inode references an allocator-free data block",
            ));
        }
    }

    let namespace_references = entries
        .iter()
        .filter(|entry| entry.target == inode_id)
        .count();
    if inode_id != 1 && namespace_references == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "resolved non-root inode has no namespace reference",
        ));
    }

    Ok(PathMetadata {
        inode_id,
        kind: inode.kind,
        logical_blocks: inode.blocks.len(),
        namespace_references,
    })
}
