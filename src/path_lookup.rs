use std::collections::VecDeque;
use std::io;

use crate::block::BlockDevice;
use crate::directory_table::load_directory_table;
use crate::format::Superblock;
use crate::inode::InodeKind;
use crate::inode_table::load_inode_table;
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
    parse_component_sequence(path.trim_start_matches('/'))
}

fn parse_target_components(target: &str) -> io::Result<VecDeque<String>> {
    if target == "/" {
        return Ok(VecDeque::new());
    }
    let body = if target.starts_with('/') {
        target.trim_start_matches('/')
    } else {
        target
    };
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
    inodes.iter().find(|inode| inode.id == inode_id).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "namespace references a missing inode",
        )
    })
}

fn invalid_input(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
