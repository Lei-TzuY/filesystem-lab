mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::rename_exchange_tx::rename_exchange_files_journaled;
use support::CrashDevice;

fn inode(id: u64, kind: InodeKind, blocks: Vec<u64>) -> PersistedInode {
    PersistedInode { id, kind, blocks }
}

fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry {
        parent,
        target,
        name: name.to_owned(),
    }
}

fn old_entries() -> Vec<PersistedDirectoryEntry> {
    vec![entry(1, 2, "alpha"), entry(1, 3, "beta")]
}

fn new_entries() -> Vec<PersistedDirectoryEntry> {
    vec![entry(1, 3, "alpha"), entry(1, 2, "beta")]
}

fn setup() -> (CrashDevice, Superblock, u64, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let first_block = allocator.allocate().unwrap();
    let second_block = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory, Vec::new()),
            inode(2, InodeKind::File, vec![first_block]),
            inode(3, InodeKind::File, vec![second_block]),
        ],
    )
    .unwrap();
    store_directory_table(&mut device, &superblock, &old_entries()).unwrap();
    device
        .write_block(first_block, &[0xA5; BLOCK_SIZE])
        .unwrap();
    device
        .write_block(second_block, &[0x5A; BLOCK_SIZE])
        .unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, first_block, second_block)
}

fn exchange(device: &mut CrashDevice, superblock: &Superblock) -> io::Result<RecoveryReport> {
    rename_exchange_files_journaled(device, superblock, 1, "alpha", 1, "beta")
}

fn assert_ownership_and_data_unchanged(
    device: &mut CrashDevice,
    superblock: &Superblock,
    first_block: u64,
    second_block: u64,
) {
    let allocator = load_allocator(device, superblock).unwrap();
    assert!(allocator.is_owned(first_block).unwrap());
    assert!(allocator.is_owned(second_block).unwrap());
    assert_eq!(
        load_inode_table(device, superblock).unwrap(),
        vec![
            inode(1, InodeKind::Directory, Vec::new()),
            inode(2, InodeKind::File, vec![first_block]),
            inode(3, InodeKind::File, vec![second_block]),
        ]
    );

    let mut first_data = [0_u8; BLOCK_SIZE];
    let mut second_data = [0_u8; BLOCK_SIZE];
    device.read_block(first_block, &mut first_data).unwrap();
    device.read_block(second_block, &mut second_data).unwrap();
    assert_eq!(first_data, [0xA5; BLOCK_SIZE]);
    assert_eq!(second_data, [0x5A; BLOCK_SIZE]);
}

#[test]
fn exchange_swaps_only_namespace_targets() {
    let (mut device, superblock, first_block, second_block) = setup();

    let report = exchange(&mut device, &superblock).unwrap();

    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 1);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        new_entries()
    );
    assert_ownership_and_data_unchanged(&mut device, &superblock, first_block, second_block);
    check_device(&mut device).unwrap();
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn every_exchange_crash_is_old_or_recoverable_new() {
    let (mut probe, superblock, first_block, second_block) = setup();
    probe.arm(None);
    exchange(&mut probe, &superblock).unwrap();
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 5);
    assert_eq!(
        load_directory_table(&mut probe, &superblock).unwrap(),
        new_entries()
    );
    assert_ownership_and_data_unchanged(&mut probe, &superblock, first_block, second_block);

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, first_block, second_block) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            exchange(&mut device, &superblock).unwrap_err().kind(),
            io::ErrorKind::Other,
            "crash point {crash_at} must interrupt rename exchange"
        );
        device.reboot();

        assert_ownership_and_data_unchanged(&mut device, &superblock, first_block, second_block);
        let visible_entries = load_directory_table(&mut device, &superblock).unwrap();
        assert!(
            visible_entries == old_entries() || visible_entries == new_entries(),
            "crash point {crash_at} exposed a partial exchange namespace"
        );
        check_device(&mut device).unwrap();

        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if report.committed_transactions == 0 {
            assert_eq!(
                load_directory_table(&mut device, &superblock).unwrap(),
                old_entries()
            );
        } else {
            assert_eq!(report.committed_transactions, 1);
            assert_eq!(report.home_writes, 1);
            assert_eq!(
                load_directory_table(&mut device, &superblock).unwrap(),
                new_entries()
            );
        }
        assert_ownership_and_data_unchanged(&mut device, &superblock, first_block, second_block);
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }
}

#[test]
fn exchange_rejects_directory_target_before_wal_publication() {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[
            inode(1, InodeKind::Directory, Vec::new()),
            inode(2, InodeKind::File, Vec::new()),
            inode(3, InodeKind::Directory, Vec::new()),
        ],
    )
    .unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[entry(1, 2, "file"), entry(1, 3, "directory")],
    )
    .unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    device.arm(None);

    assert_eq!(
        rename_exchange_files_journaled(&mut device, &superblock, 1, "file", 1, "directory")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(device.operations(), 0);
    assert_eq!(
        load_directory_table(&mut device, &superblock).unwrap(),
        vec![entry(1, 2, "file"), entry(1, 3, "directory")]
    );
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}
