use std::collections::VecDeque;
use std::io;

use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::file_data::write_file_range_journaled;
use crate::file_range_read::read_file_range;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
use crate::recovery::RecoveryReport;
use crate::symlink::read_symlink;

pub const MAX_SYMLINK_EXPANSIONS: usize = 40;

/// Resolves one absolute path from the root inode while following bounded symbolic links.
///
/// Symlink targets may be absolute or relative. Relative targets are interpreted against the
/// directory containing the symlink, while any unconsumed suffix of the original path is preserved.
/// `.` and `..` are deliberately rejected rather than normalized so this helper has one explicit,
/// bounded traversal contract.
///
/// # Errors
/// Returns `InvalidInput` for a non-absolute or malformed path and for traversal through a
/// non-directory inode, `NotFound` for a missing namespace component, `InvalidData` for dangling
/// inode references or excessive symlink expansion, and propagates persisted metadata or symlink
/// payload corruption.
pub fn resolve_path_following_symlinks(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<u64> {
    resolve_path(device, superblock, path, true)
}

/// Resolves one absolute path while following intermediate symbolic links but not the final one.
///
/// This is the lookup primitive needed by `readlink`-style operations: if the final namespace entry
/// names a symbolic-link inode, the inode itself is returned even when its target is dangling.
/// Intermediate symlinks retain the same bounded expansion and validation contract as
/// [`resolve_path_following_symlinks`].
///
/// # Errors
/// Returns the same path and consistency errors as [`resolve_path_following_symlinks`], except that
/// the final symbolic-link payload is not read merely to resolve its inode.
pub fn resolve_path_without_following_final_symlink(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<u64> {
    resolve_path(device, superblock, path, false)
}

/// Reads the persisted target of the symbolic link named by one absolute pathname.
///
/// Intermediate symbolic links are followed, but the final component is resolved without following
/// it. The final inode must itself be a symbolic link; its persisted `SYM1` payload is then validated
/// by [`read_symlink`]. A dangling target is therefore readable, matching `readlink`-style semantics.
///
/// # Errors
/// Propagates bounded pathname lookup errors and returns `InvalidInput` when the final inode is not
/// a symbolic link. Corrupt symbolic-link payloads return `InvalidData`.
pub fn read_symlink_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
) -> io::Result<String> {
    let inode_id = resolve_path_without_following_final_symlink(device, superblock, path)?;
    read_symlink(device, superblock, inode_id)
}

/// Reads one bounded byte range from the regular file named by an absolute pathname.
///
/// Path resolution follows symbolic links, including the final component, using the same bounded
/// expansion rules as [`resolve_path_following_symlinks`]. The resolved inode is then passed to the
/// existing inode-ID-based [`read_file_range`] implementation, so allocator ownership checks and
/// format-v5 block-range bounds stay centralized in one data-path primitive.
///
/// Format v5 has no persisted byte length. This operation therefore exposes only byte ranges inside
/// logical blocks already referenced by the resolved regular-file inode; it does not define EOF,
/// sparse-hole, allocation, or extension semantics.
///
/// # Errors
/// Propagates pathname lookup errors and all [`read_file_range`] validation errors, including a
/// resolved non-file inode, invalid offsets/ranges, and allocator ownership disagreement.
pub fn read_file_range_at_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    first_block_index: usize,
    start_offset: usize,
    len: usize,
) -> io::Result<Vec<u8>> {
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    read_file_range(
        device,
        superblock,
        inode_id,
        first_block_index,
        start_offset,
        len,
    )
}

/// Atomically writes one bounded byte range to the regular file named by an absolute pathname.
///
/// Path resolution follows symbolic links, including the final component, using the same bounded
/// expansion rules as [`resolve_path_following_symlinks`]. The resolved inode is then passed directly
/// to [`write_file_range_journaled`], keeping regular-file kind, range, allocator ownership, WAL
/// publication, recovery, and journal-capacity validation centralized in the existing mutation path.
///
/// This operation only overwrites bytes inside logical blocks already referenced by the resolved
/// regular-file inode. It does not allocate blocks, extend the file, define persisted EOF semantics,
/// or mutate allocator, inode, or namespace metadata.
///
/// # Errors
/// Propagates pathname lookup errors and all [`write_file_range_journaled`] validation or durable I/O
/// errors, including a resolved non-file inode, empty/out-of-range writes, ownership disagreement,
/// and insufficient journal capacity.
pub fn write_file_range_at_path_journaled(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    first_block_index: usize,
    start_offset: usize,
    data: &[u8],
) -> io::Result<RecoveryReport> {
    let inode_id = resolve_path_following_symlinks(device, superblock, path)?;
    write_file_range_journaled(
        device,
        superblock,
        inode_id,
        first_block_index,
        start_offset,
        data,
    )
}

fn resolve_path(
    device: &mut impl BlockDevice,
    superblock: &Superblock,
    path: &str,
    follow_final_symlink: bool,
) -> io::Result<u64> {
    if !path.starts_with('/') {
        return Err(invalid_input("path must be absolute"));
    }

    let inodes = load_inode_table(device, superblock)?;
    let entries = load_directory_table(device, superblock)?;
    let mut pending = parse_components(path)?;
    let mut current = 1_u64;
    let mut symlink_expansions = 0_usize;

    if pending.is_empty() {
        return require_inode(&inodes, current).map(|inode| inode.id);
    }

    while let Some(component) = pending.pop_front() {
        let current_inode = require_inode(&inodes, current)?;
        if current_inode.kind != InodeKind::Directory {
            return Err(invalid_input("path traversal requires a directory inode"));
        }

        let entry = entries
            .iter()
            .find(|entry| entry.parent == current && entry.name == component)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "path component not found"))?;
        let target_inode = require_inode(&inodes, entry.target)?;

        if target_inode.kind == InodeKind::Symlink {
            if pending.is_empty() && !follow_final_symlink {
                return Ok(target_inode.id);
            }

            symlink_expansions += 1;
            if symlink_expansions > MAX_SYMLINK_EXPANSIONS {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "symbolic-link expansion limit exceeded",
                ));
            }

            let target = read_symlink(device, superblock, target_inode.id)?;
            let target_is_absolute = target.starts_with('/');
            let mut expanded = parse_target_components(&target)?;
            expanded.append(&mut pending);
            pending = expanded;
            if target_is_absolute {
                current = 1;
            }
            continue;
        }

        current = target_inode.id;
    }

    Ok(current)
}

fn parse_components(path: &str) -> io::Result<VecDeque<String>> {
    if path == "/" {
        return Ok(VecDeque::new());
    }
    if path.ends_with('/') {
        return Err(invalid_input("non-root path must not end with '/'"));
    }
    let body = path
        .strip_prefix('/')
        .ok_or_else(|| invalid_input("path must be absolute"))?;
    parse_component_sequence(body)
}

fn parse_target_components(target: &str) -> io::Result<VecDeque<String>> {
    if target == "/" {
        return Ok(VecDeque::new());
    }
    let body = target.strip_prefix('/').unwrap_or(target);
    if target.ends_with('/') {
        return Err(invalid_input("symlink target must not end with '/'"));
    }
    parse_component_sequence(body)
}

fn parse_component_sequence(sequence: &str) -> io::Result<VecDeque<String>> {
    if sequence.is_empty() {
        return Err(invalid_input("path contains an empty component"));
    }
    let mut components = VecDeque::new();
    for component in sequence.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(invalid_input("path contains an unsupported component"));
        }
        components.push_back(component.to_owned());
    }
    Ok(components)
}

fn require_inode(
    inodes: &[crate::inode_codec::PersistedInode],
    inode_id: u64,
) -> io::Result<&crate::inode_codec::PersistedInode> {
    inodes
        .iter()
        .find(|inode| inode.id == inode_id)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "namespace references a missing inode",
            )
        })
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
