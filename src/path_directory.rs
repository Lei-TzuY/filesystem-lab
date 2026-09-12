use std::io;

use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::journal_checkpoint::recover_journal_and_checkpoint;
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
/// Before resolving the pathname, any older committed WAL transaction is recovered and checkpointed
/// so directory selection and enumeration are derived from the recovered namespace. Path resolution
/// then follows intermediate and final symbolic links using the existing bounded resolver. The
/// resolved inode must be a persisted directory. Child entries are derived from the durable directory
/// table and joined against the durable inode table so callers receive the exact child inode identity
/// and kind without following child symlinks. Results are sorted by entry name for deterministic
/// enumeration independent of directory-table record order.
///
/// This is deliberately a read-only format-v5 namespace surface. It does not synthesize `.` or
/// `..`, persist directory ordering, or claim POSIX readdir ordering semantics.
///
/// # Errors
///
/// Propagates recovery/checkpoint, bounded pathname lookup, and persisted table decode errors. Returns
/// `InvalidInput` when the resolved pathname is not a directory, and `InvalidData` when a durable
/// directory entry names an inode that is absent from the inode table.
pub fn list_directory_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<Vec<PathDirectoryEntry>> {
    recover_journal_and_checkpoint(device, *superblock)?;
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

/// Lists one deterministic bounded page of immediate children for a directory pathname.
///
/// Pagination is applied after the same recovery, pathname resolution, durable namespace validation,
/// and deterministic name ordering as [`list_directory_at_path`]. `offset` counts entries in that
/// sorted snapshot and `limit` bounds only the returned vector; an offset at or beyond the end, or a
/// zero limit, returns an empty page. The offset is intentionally a snapshot-relative index rather
/// than a durable readdir cookie: concurrent namespace mutation between calls can move entries across
/// page boundaries.
///
/// No on-disk state or format semantics are changed.
///
/// # Errors
///
/// Propagates the same recovery/checkpoint, pathname lookup, table decoding, target-kind, and durable
/// namespace consistency errors as [`list_directory_at_path`].
pub fn list_directory_page_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    offset: usize,
    limit: usize,
) -> io::Result<Vec<PathDirectoryEntry>> {
    let entries = list_directory_at_path(device, superblock, path)?;
    Ok(entries.into_iter().skip(offset).take(limit).collect())
}

/// Lists one deterministic bounded page starting strictly after an entry name.
///
/// The caller-provided `after_name` is a lexical cursor over the same deterministic name ordering as
/// [`list_directory_at_path`]. The cursor does not need to name a currently existing child: entries
/// whose names compare strictly greater than the cursor are eligible for the page. Passing `None`
/// starts at the first entry, while a zero `limit` returns an empty page after performing the normal
/// recovery and namespace validation.
///
/// A name cursor avoids the positional shift of offset pagination when entries are inserted before a
/// previously observed boundary, but it is still not a durable POSIX readdir cookie. Renames,
/// deletions, or insertion of names at or after the cursor between calls can change later pages. The
/// implementation currently validates and sorts the complete recovered directory snapshot before
/// applying the cursor and limit; it does not claim indexed on-disk directory scaling.
///
/// No on-disk state or format semantics are changed.
///
/// # Errors
///
/// Propagates the same recovery/checkpoint, pathname lookup, table decoding, target-kind, and durable
/// namespace consistency errors as [`list_directory_at_path`].
pub fn list_directory_page_after_name_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    after_name: Option<&str>,
    limit: usize,
) -> io::Result<Vec<PathDirectoryEntry>> {
    let entries = list_directory_at_path(device, superblock, path)?;
    Ok(entries
        .into_iter()
        .filter(|entry| after_name.is_none_or(|cursor| entry.name.as_str() > cursor))
        .take(limit)
        .collect())
}

fn invalid_data(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
