use std::io;

use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::path_lookup::resolve_path_following_symlinks;

/// One durable child entry returned by [`list_directory_at_path`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathDirectoryEntry {
    pub name: String,
    pub inode_id: u64,
    pub kind: InodeKind,
}

/// Lists the immediate children of one absolute directory pathname.
///
/// Path resolution follows intermediate and final symbolic links using the existing bounded
/// resolver. The resolved inode must be a persisted directory. Child entries are derived from the
/// durable directory table and joined against the durable inode table so callers receive the exact
/// child inode identity and kind without following child symlinks. Results are sorted by entry name
/// for deterministic enumeration independent of directory-table record order.
///
/// This is deliberately a read-only format-v5 namespace surface. It does not synthesize `.` or
/// `..`, expose cookies/offsets, or claim POSIX readdir ordering semantics.
///
/// # Errors
///
/// Propagates bounded pathname lookup and persisted table decode errors. Returns `InvalidInput` when
/// the resolved pathname is not a directory, and `InvalidData` when a durable directory entry names
/// an inode that is absent from the inode table.
pub fn list_directory_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<Vec<PathDirectoryEntry>> {
    let directory_inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    let inodes = load_inode_table(device, superblock)?;
    let directory_inode = inodes
        .iter()
        .find(|inode| inode.id == directory_inode_id)
        .ok_or_else(|| invalid_data("resolved directory inode is missing from inode table"))?;
    if directory_inode.kind != InodeKind::Directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "resolved pathname is not a directory",
        ));
    }

    let directory_entries = load_directory_table(device, superblock)?;
    let mut result = Vec::new();
    for entry in directory_entries
        .into_iter()
        .filter(|entry| entry.parent == directory_inode_id)
    {
        let target = inodes
            .iter()
            .find(|inode| inode.id == entry.target)
            .ok_or_else(|| invalid_data("directory entry references missing inode"))?;
        result.push(PathDirectoryEntry {
            name: entry.name,
            inode_id: entry.target,
            kind: target.kind,
        });
    }
    result.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(result)
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
