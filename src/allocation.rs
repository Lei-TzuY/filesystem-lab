use std::{error::Error, fmt};

/// Errors returned by the in-memory block allocator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationError {
    InvalidLayout {
        total_blocks: u64,
        reserved_blocks: u64,
    },
    AddressSpaceTooLarge(u64),
    OutOfRange {
        block: u64,
        total_blocks: u64,
    },
    ReservedBlock(u64),
    AlreadyFree(u64),
    Exhausted,
}

impl fmt::Display for AllocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::InvalidLayout {
                total_blocks,
                reserved_blocks,
            } => write!(
                formatter,
                "invalid allocation layout: {reserved_blocks} reserved blocks exceed {total_blocks} total blocks"
            ),
            Self::AddressSpaceTooLarge(total_blocks) => write!(
                formatter,
                "allocation bitmap cannot represent {total_blocks} blocks on this platform"
            ),
            Self::OutOfRange {
                block,
                total_blocks,
            } => write!(
                formatter,
                "block {block} is outside allocator range 0..{total_blocks}"
            ),
            Self::ReservedBlock(block) => write!(formatter, "block {block} is reserved metadata"),
            Self::AlreadyFree(block) => write!(formatter, "block {block} is already free"),
            Self::Exhausted => formatter.write_str("no free data blocks remain"),
        }
    }
}

impl Error for AllocationError {}

/// Executable invariant violations for [`BlockAllocator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocationInvariantError {
    AllocatedCountMismatch {
        recorded: u64,
        observed: u64,
    },
    AccountingOverflow,
    AccountingMismatch {
        total_blocks: u64,
        reserved_blocks: u64,
        allocated_blocks: u64,
        free_blocks: u64,
    },
}

impl fmt::Display for AllocationInvariantError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::AllocatedCountMismatch { recorded, observed } => write!(
                formatter,
                "allocated-block count mismatch: recorded {recorded}, observed {observed}"
            ),
            Self::AccountingOverflow => formatter.write_str("allocation accounting overflow"),
            Self::AccountingMismatch {
                total_blocks,
                reserved_blocks,
                allocated_blocks,
                free_blocks,
            } => write!(
                formatter,
                "allocation accounting mismatch: total={total_blocks}, reserved={reserved_blocks}, allocated={allocated_blocks}, free={free_blocks}"
            ),
        }
    }
}

impl Error for AllocationInvariantError {}

/// Deterministic first-fit allocator for filesystem blocks.
///
/// Reserved metadata blocks occupy the prefix `0..reserved_blocks` and are never returned by
/// [`BlockAllocator::allocate`]. The remaining blocks are tracked in-memory only; durable bitmap
/// encoding is intentionally deferred to a future on-disk format revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAllocator {
    total_blocks: u64,
    reserved_blocks: u64,
    allocated: Vec<bool>,
    allocated_blocks: u64,
}

impl BlockAllocator {
    /// Creates an allocator for `total_blocks` with a reserved metadata prefix.
    ///
    /// # Errors
    ///
    /// Returns [`AllocationError::InvalidLayout`] when the reserved prefix exceeds the device and
    /// [`AllocationError::AddressSpaceTooLarge`] when the block count cannot be indexed on this
    /// platform.
    pub fn new(total_blocks: u64, reserved_blocks: u64) -> Result<Self, AllocationError> {
        if reserved_blocks > total_blocks {
            return Err(AllocationError::InvalidLayout {
                total_blocks,
                reserved_blocks,
            });
        }

        let length = usize::try_from(total_blocks)
            .map_err(|_| AllocationError::AddressSpaceTooLarge(total_blocks))?;

        Ok(Self {
            total_blocks,
            reserved_blocks,
            allocated: vec![false; length],
            allocated_blocks: 0,
        })
    }

    #[must_use]
    pub const fn total_blocks(&self) -> u64 {
        self.total_blocks
    }

    #[must_use]
    pub const fn reserved_blocks(&self) -> u64 {
        self.reserved_blocks
    }

    #[must_use]
    pub const fn allocated_blocks(&self) -> u64 {
        self.allocated_blocks
    }

    #[must_use]
    pub const fn free_blocks(&self) -> u64 {
        self.total_blocks - self.reserved_blocks - self.allocated_blocks
    }

    /// Allocates the lowest-numbered free data block.
    ///
    /// # Errors
    ///
    /// Returns [`AllocationError::Exhausted`] when no free data block remains.
    pub fn allocate(&mut self) -> Result<u64, AllocationError> {
        let start = usize::try_from(self.reserved_blocks)
            .map_err(|_| AllocationError::AddressSpaceTooLarge(self.total_blocks))?;

        let Some(index) = self.allocated[start..]
            .iter()
            .position(|allocated| !allocated)
        else {
            return Err(AllocationError::Exhausted);
        };
        let absolute_index = start + index;
        self.allocated[absolute_index] = true;
        self.allocated_blocks += 1;

        u64::try_from(absolute_index)
            .map_err(|_| AllocationError::AddressSpaceTooLarge(self.total_blocks))
    }

    /// Frees a previously allocated data block.
    ///
    /// # Errors
    ///
    /// Returns an error for out-of-range blocks, reserved metadata blocks, or double-free attempts.
    pub fn free(&mut self, block: u64) -> Result<(), AllocationError> {
        let index = self.data_index(block)?;
        if !self.allocated[index] {
            return Err(AllocationError::AlreadyFree(block));
        }

        self.allocated[index] = false;
        self.allocated_blocks -= 1;
        Ok(())
    }

    /// Reports whether a block is unavailable because it is reserved or allocated.
    ///
    /// # Errors
    ///
    /// Returns [`AllocationError::OutOfRange`] when `block` lies outside the allocator.
    pub fn is_owned(&self, block: u64) -> Result<bool, AllocationError> {
        self.ensure_in_range(block)?;
        if block < self.reserved_blocks {
            return Ok(true);
        }
        let index = usize::try_from(block)
            .map_err(|_| AllocationError::AddressSpaceTooLarge(self.total_blocks))?;
        Ok(self.allocated[index])
    }

    /// Checks allocator accounting invariants against the bitmap contents.
    ///
    /// # Errors
    ///
    /// Returns an invariant error when the recorded allocation count disagrees with the bitmap or
    /// when `reserved + allocated + free != total`.
    pub fn validate(&self) -> Result<(), AllocationInvariantError> {
        let start = usize::try_from(self.reserved_blocks)
            .map_err(|_| AllocationInvariantError::AccountingOverflow)?;
        let observed = u64::try_from(
            self.allocated[start..]
                .iter()
                .filter(|allocated| **allocated)
                .count(),
        )
        .map_err(|_| AllocationInvariantError::AccountingOverflow)?;

        if observed != self.allocated_blocks {
            return Err(AllocationInvariantError::AllocatedCountMismatch {
                recorded: self.allocated_blocks,
                observed,
            });
        }

        let accounted = self
            .reserved_blocks
            .checked_add(self.allocated_blocks)
            .and_then(|value| value.checked_add(self.free_blocks()))
            .ok_or(AllocationInvariantError::AccountingOverflow)?;
        if accounted != self.total_blocks {
            return Err(AllocationInvariantError::AccountingMismatch {
                total_blocks: self.total_blocks,
                reserved_blocks: self.reserved_blocks,
                allocated_blocks: self.allocated_blocks,
                free_blocks: self.free_blocks(),
            });
        }
        Ok(())
    }

    fn ensure_in_range(&self, block: u64) -> Result<(), AllocationError> {
        if block >= self.total_blocks {
            return Err(AllocationError::OutOfRange {
                block,
                total_blocks: self.total_blocks,
            });
        }
        Ok(())
    }

    fn data_index(&self, block: u64) -> Result<usize, AllocationError> {
        self.ensure_in_range(block)?;
        if block < self.reserved_blocks {
            return Err(AllocationError::ReservedBlock(block));
        }
        usize::try_from(block).map_err(|_| AllocationError::AddressSpaceTooLarge(self.total_blocks))
    }
}
