use std::{collections::BTreeMap, error::Error, fmt};

use crate::allocation::{AllocationError, BlockAllocator};

/// Stable identifier for an inode in the in-memory model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InodeId(u64);

impl InodeId {
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Minimal inode kinds needed before directory-entry semantics are introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InodeKind {
    File,
    Directory,
}

/// In-memory inode state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inode {
    id: InodeId,
    kind: InodeKind,
    blocks: Vec<u64>,
}

impl Inode {
    #[must_use]
    pub const fn id(&self) -> InodeId {
        self.id
    }

    #[must_use]
    pub const fn kind(&self) -> InodeKind {
        self.kind
    }

    #[must_use]
    pub fn blocks(&self) -> &[u64] {
        &self.blocks
    }
}

/// Errors returned by inode lifecycle operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InodeError {
    IdExhausted,
    NotFound(InodeId),
    BlockNotAllocated(u64),
    ReservedBlock(u64),
    BlockAlreadyOwned {
        block: u64,
        owner: InodeId,
    },
    BlockNotOwnedByInode {
        inode: InodeId,
        block: u64,
    },
    InodeStillOwnsBlocks(InodeId),
    Allocation(AllocationError),
}

impl fmt::Display for InodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::IdExhausted => formatter.write_str("inode identifier space exhausted"),
            Self::NotFound(id) => write!(formatter, "inode {} does not exist", id.get()),
            Self::BlockNotAllocated(block) => {
                write!(formatter, "block {block} is not allocated to filesystem data")
            }
            Self::ReservedBlock(block) => write!(formatter, "block {block} is reserved metadata"),
            Self::BlockAlreadyOwned { block, owner } => write!(
                formatter,
                "block {block} is already owned by inode {}",
                owner.get()
            ),
            Self::BlockNotOwnedByInode { inode, block } => write!(
                formatter,
                "block {block} is not owned by inode {}",
                inode.get()
            ),
            Self::InodeStillOwnsBlocks(id) => write!(
                formatter,
                "inode {} cannot be removed while it still owns blocks",
                id.get()
            ),
            Self::Allocation(error) => write!(formatter, "allocator rejected inode operation: {error}"),
        }
    }
}

impl Error for InodeError {}

impl From<AllocationError> for InodeError {
    fn from(error: AllocationError) -> Self {
        Self::Allocation(error)
    }
}

/// Executable invariant violations for [`InodeTable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InodeInvariantError {
    BlockOwnerMissingInode {
        block: u64,
        owner: InodeId,
    },
    ReverseOwnershipMismatch {
        block: u64,
        owner: InodeId,
    },
    DuplicateBlockReference {
        block: u64,
        first_owner: InodeId,
        second_owner: InodeId,
    },
    ReservedBlockReferenced {
        inode: InodeId,
        block: u64,
    },
    UnallocatedBlockReferenced {
        inode: InodeId,
        block: u64,
    },
    Allocation(AllocationError),
}

impl fmt::Display for InodeInvariantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::BlockOwnerMissingInode { block, owner } => write!(
                formatter,
                "ownership index maps block {block} to missing inode {}",
                owner.get()
            ),
            Self::ReverseOwnershipMismatch { block, owner } => write!(
                formatter,
                "ownership index for block {block} disagrees with inode {}",
                owner.get()
            ),
            Self::DuplicateBlockReference {
                block,
                first_owner,
                second_owner,
            } => write!(
                formatter,
                "block {block} is referenced by both inode {} and inode {}",
                first_owner.get(),
                second_owner.get()
            ),
            Self::ReservedBlockReferenced { inode, block } => write!(
                formatter,
                "inode {} references reserved block {block}",
                inode.get()
            ),
            Self::UnallocatedBlockReferenced { inode, block } => write!(
                formatter,
                "inode {} references unallocated block {block}",
                inode.get()
            ),
            Self::Allocation(error) => write!(formatter, "allocator validation failed: {error}"),
        }
    }
}

impl Error for InodeInvariantError {}

/// Deterministic in-memory inode table with explicit block ownership tracking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InodeTable {
    next_id: u64,
    inodes: BTreeMap<InodeId, Inode>,
    block_owners: BTreeMap<u64, InodeId>,
}

impl Default for InodeTable {
    fn default() -> Self {
        Self::new()
    }
}

impl InodeTable {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_id: 1,
            inodes: BTreeMap::new(),
            block_owners: BTreeMap::new(),
        }
    }

    /// Creates a new inode with a monotonically increasing identifier.
    ///
    /// # Errors
    ///
    /// Returns [`InodeError::IdExhausted`] when no further identifier can be assigned.
    pub fn create(&mut self, kind: InodeKind) -> Result<InodeId, InodeError> {
        let id = InodeId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or(InodeError::IdExhausted)?;
        self.inodes.insert(
            id,
            Inode {
                id,
                kind,
                blocks: Vec::new(),
            },
        );
        Ok(id)
    }

    #[must_use]
    pub fn get(&self, id: InodeId) -> Option<&Inode> {
        self.inodes.get(&id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inodes.is_empty()
    }

    /// Attaches an already allocated data block to an inode.
    ///
    /// # Errors
    ///
    /// Returns an error when the inode does not exist, the block is reserved or unallocated, the
    /// block lies outside the allocator, or another inode already owns it.
    pub fn attach_block(
        &mut self,
        inode: InodeId,
        block: u64,
        allocator: &BlockAllocator,
    ) -> Result<(), InodeError> {
        if !self.inodes.contains_key(&inode) {
            return Err(InodeError::NotFound(inode));
        }
        if block < allocator.reserved_blocks() {
            return Err(InodeError::ReservedBlock(block));
        }
        if !allocator.is_owned(block)? {
            return Err(InodeError::BlockNotAllocated(block));
        }
        if let Some(owner) = self.block_owners.get(&block).copied() {
            return Err(InodeError::BlockAlreadyOwned { block, owner });
        }

        self.inodes
            .get_mut(&inode)
            .expect("inode presence checked above")
            .blocks
            .push(block);
        self.block_owners.insert(block, inode);
        Ok(())
    }

    /// Detaches a block from an inode without freeing it in the allocator.
    ///
    /// # Errors
    ///
    /// Returns an error when the inode is missing or does not own the requested block.
    pub fn detach_block(&mut self, inode: InodeId, block: u64) -> Result<(), InodeError> {
        let node = self.inodes.get_mut(&inode).ok_or(InodeError::NotFound(inode))?;
        let Some(position) = node.blocks.iter().position(|candidate| *candidate == block) else {
            return Err(InodeError::BlockNotOwnedByInode { inode, block });
        };

        node.blocks.remove(position);
        self.block_owners.remove(&block);
        Ok(())
    }

    /// Removes an inode after all block ownership has been detached.
    ///
    /// # Errors
    ///
    /// Returns an error when the inode is missing or still owns data blocks.
    pub fn remove(&mut self, inode: InodeId) -> Result<Inode, InodeError> {
        let node = self.inodes.get(&inode).ok_or(InodeError::NotFound(inode))?;
        if !node.blocks.is_empty() {
            return Err(InodeError::InodeStillOwnsBlocks(inode));
        }
        self.inodes.remove(&inode).ok_or(InodeError::NotFound(inode))
    }

    /// Validates inode/block ownership against the allocator and reverse ownership index.
    ///
    /// # Errors
    ///
    /// Returns an invariant error for duplicate references, references to reserved or free blocks,
    /// missing inode owners, or disagreement between forward and reverse ownership state.
    pub fn validate(&self, allocator: &BlockAllocator) -> Result<(), InodeInvariantError> {
        let mut observed = BTreeMap::new();
        for (inode_id, inode) in &self.inodes {
            for &block in &inode.blocks {
                if block < allocator.reserved_blocks() {
                    return Err(InodeInvariantError::ReservedBlockReferenced {
                        inode: *inode_id,
                        block,
                    });
                }
                match allocator.is_owned(block) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(InodeInvariantError::UnallocatedBlockReferenced {
                            inode: *inode_id,
                            block,
                        });
                    }
                    Err(error) => return Err(InodeInvariantError::Allocation(error)),
                }
                if let Some(first_owner) = observed.insert(block, *inode_id) {
                    return Err(InodeInvariantError::DuplicateBlockReference {
                        block,
                        first_owner,
                        second_owner: *inode_id,
                    });
                }
                if self.block_owners.get(&block) != Some(inode_id) {
                    return Err(InodeInvariantError::ReverseOwnershipMismatch {
                        block,
                        owner: *inode_id,
                    });
                }
            }
        }

        for (&block, &owner) in &self.block_owners {
            let Some(inode) = self.inodes.get(&owner) else {
                return Err(InodeInvariantError::BlockOwnerMissingInode { block, owner });
            };
            if !inode.blocks.contains(&block) {
                return Err(InodeInvariantError::ReverseOwnershipMismatch { block, owner });
            }
        }
        Ok(())
    }
}
