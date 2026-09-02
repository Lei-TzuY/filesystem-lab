use std::{collections::BTreeMap, error::Error, fmt};

use crate::inode::{InodeId, InodeKind, InodeTable};

/// Errors returned by directory namespace operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryError {
    ParentNotFound(InodeId),
    ParentNotDirectory(InodeId),
    TargetNotFound(InodeId),
    InvalidName(String),
    EntryAlreadyExists { parent: InodeId, name: String },
    EntryNotFound { parent: InodeId, name: String },
}

impl fmt::Display for DirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParentNotFound(parent) => {
                write!(formatter, "parent inode {} does not exist", parent.get())
            }
            Self::ParentNotDirectory(parent) => {
                write!(
                    formatter,
                    "parent inode {} is not a directory",
                    parent.get()
                )
            }
            Self::TargetNotFound(target) => {
                write!(formatter, "target inode {} does not exist", target.get())
            }
            Self::InvalidName(name) => write!(formatter, "invalid directory entry name {name:?}"),
            Self::EntryAlreadyExists { parent, name } => write!(
                formatter,
                "directory inode {} already contains entry {name:?}",
                parent.get()
            ),
            Self::EntryNotFound { parent, name } => write!(
                formatter,
                "directory inode {} does not contain entry {name:?}",
                parent.get()
            ),
        }
    }
}

impl Error for DirectoryError {}

/// Executable namespace invariant violations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectoryInvariantError {
    ParentMissing(InodeId),
    ParentNotDirectory(InodeId),
    InvalidName {
        parent: InodeId,
        name: String,
    },
    DanglingTarget {
        parent: InodeId,
        name: String,
        target: InodeId,
    },
}

impl fmt::Display for DirectoryInvariantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ParentMissing(parent) => {
                write!(
                    formatter,
                    "namespace contains missing parent inode {}",
                    parent.get()
                )
            }
            Self::ParentNotDirectory(parent) => write!(
                formatter,
                "namespace parent inode {} is not a directory",
                parent.get()
            ),
            Self::InvalidName { parent, name } => write!(
                formatter,
                "directory inode {} contains invalid entry name {name:?}",
                parent.get()
            ),
            Self::DanglingTarget {
                parent,
                name,
                target,
            } => write!(
                formatter,
                "directory inode {} entry {name:?} references missing inode {}",
                parent.get(),
                target.get()
            ),
        }
    }
}

impl Error for DirectoryInvariantError {}

/// Deterministic in-memory directory namespace keyed by parent inode and entry name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirectoryTable {
    entries: BTreeMap<InodeId, BTreeMap<String, InodeId>>,
}

impl DirectoryTable {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
        }
    }

    /// Inserts a directory entry after validating both inode endpoints and the component name.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent is missing or is not a directory, the target inode is
    /// missing, the name is not a valid single path component, or the name already exists.
    pub fn insert(
        &mut self,
        parent: InodeId,
        name: &str,
        target: InodeId,
        inodes: &InodeTable,
    ) -> Result<(), DirectoryError> {
        validate_parent(parent, inodes)?;
        if inodes.get(target).is_none() {
            return Err(DirectoryError::TargetNotFound(target));
        }
        if !valid_name(name) {
            return Err(DirectoryError::InvalidName(name.to_owned()));
        }

        let directory = self.entries.entry(parent).or_default();
        if directory.contains_key(name) {
            return Err(DirectoryError::EntryAlreadyExists {
                parent,
                name: name.to_owned(),
            });
        }
        directory.insert(name.to_owned(), target);
        Ok(())
    }

    /// Removes and returns an existing directory entry target.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent is missing or is not a directory, the name is invalid, or
    /// no such entry exists.
    pub fn remove(
        &mut self,
        parent: InodeId,
        name: &str,
        inodes: &InodeTable,
    ) -> Result<InodeId, DirectoryError> {
        validate_parent(parent, inodes)?;
        if !valid_name(name) {
            return Err(DirectoryError::InvalidName(name.to_owned()));
        }

        let Some(directory) = self.entries.get_mut(&parent) else {
            return Err(DirectoryError::EntryNotFound {
                parent,
                name: name.to_owned(),
            });
        };
        let Some(target) = directory.remove(name) else {
            return Err(DirectoryError::EntryNotFound {
                parent,
                name: name.to_owned(),
            });
        };
        if directory.is_empty() {
            self.entries.remove(&parent);
        }
        Ok(target)
    }

    #[must_use]
    pub fn lookup(&self, parent: InodeId, name: &str) -> Option<InodeId> {
        self.entries
            .get(&parent)
            .and_then(|directory| directory.get(name))
            .copied()
    }

    #[must_use]
    pub fn entry_count(&self, parent: InodeId) -> usize {
        self.entries.get(&parent).map_or(0, BTreeMap::len)
    }

    /// Validates namespace parents, path-component names, and target inode liveness.
    ///
    /// # Errors
    ///
    /// Returns an invariant error for missing or non-directory parents, malformed names, or
    /// dangling inode references.
    pub fn validate(&self, inodes: &InodeTable) -> Result<(), DirectoryInvariantError> {
        for (&parent, directory) in &self.entries {
            let Some(parent_inode) = inodes.get(parent) else {
                return Err(DirectoryInvariantError::ParentMissing(parent));
            };
            if parent_inode.kind() != InodeKind::Directory {
                return Err(DirectoryInvariantError::ParentNotDirectory(parent));
            }

            for (name, &target) in directory {
                if !valid_name(name) {
                    return Err(DirectoryInvariantError::InvalidName {
                        parent,
                        name: name.clone(),
                    });
                }
                if inodes.get(target).is_none() {
                    return Err(DirectoryInvariantError::DanglingTarget {
                        parent,
                        name: name.clone(),
                        target,
                    });
                }
            }
        }
        Ok(())
    }
}

fn validate_parent(parent: InodeId, inodes: &InodeTable) -> Result<(), DirectoryError> {
    let Some(inode) = inodes.get(parent) else {
        return Err(DirectoryError::ParentNotFound(parent));
    };
    if inode.kind() != InodeKind::Directory {
        return Err(DirectoryError::ParentNotDirectory(parent));
    }
    Ok(())
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}
