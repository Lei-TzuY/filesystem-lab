use std::collections::BTreeMap;
use std::io;

use crate::block::{BlockDevice, BLOCK_SIZE};
use crate::format::read_superblock;
use crate::fsck::{check_device, FsckReport};
use crate::journal::JournalEntry;
use crate::journal_region::load_journal_image;
use crate::recovery::{plan_recovery, RecoveryReport};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryProjectionReport {
    pub recovery: RecoveryReport,
    pub fsck: FsckReport,
}

/// Checks the filesystem state that committed WAL replay would expose without mutating the device.
///
/// The durable journal image is validated through the normal journal-region loader, then interpreted
/// through the same recovery plan used by the real journal recovery path. Committed home writes are
/// overlaid only in memory. Strict read-only fsck runs against that projected block view, while an
/// incomplete trailing transaction remains ignored exactly as it would be by real recovery.
///
/// This provides a semantic preflight for callers that need to know whether structurally valid WAL
/// replay would converge to a complete allocator/inode/namespace state before issuing any home write.
///
/// # Errors
///
/// Propagates durable superblock/journal decode errors and projected fsck failures. The projection
/// device rejects writes and flushes, so this function cannot intentionally cross a mutation or
/// durability boundary.
pub fn check_device_after_recovery_projection(
    device: &mut impl BlockDevice,
) -> io::Result<RecoveryProjectionReport> {
    let superblock = read_superblock(device)?;
    let entries = load_journal_image(device, superblock)?;
    check_device_after_entries_projection(device, &entries)
}

pub(crate) fn check_device_after_entries_projection(
    device: &mut impl BlockDevice,
    entries: &[JournalEntry],
) -> io::Result<RecoveryProjectionReport> {
    let plan = plan_recovery(entries)?;

    let mut blocks = BTreeMap::new();
    for (block, data) in plan.writes() {
        blocks.insert(*block, *data);
    }

    let recovery = plan.report();
    let mut projected = ProjectionDevice {
        base: device,
        blocks,
    };
    let fsck = check_device(&mut projected)?;

    Ok(RecoveryProjectionReport { recovery, fsck })
}

struct ProjectionDevice<'a, D: BlockDevice + ?Sized> {
    base: &'a mut D,
    blocks: BTreeMap<u64, [u8; BLOCK_SIZE]>,
}

impl<D: BlockDevice + ?Sized> BlockDevice for ProjectionDevice<'_, D> {
    fn block_count(&self) -> u64 {
        self.base.block_count()
    }

    fn read_block(&mut self, block: u64, buf: &mut [u8; BLOCK_SIZE]) -> io::Result<()> {
        if let Some(data) = self.blocks.get(&block) {
            *buf = *data;
            return Ok(());
        }
        self.base.read_block(block, buf)
    }

    fn write_block(&mut self, _block: u64, _buf: &[u8; BLOCK_SIZE]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "recovery projection is read-only",
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "recovery projection cannot flush",
        ))
    }
}
