mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::BlockDevice;
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
use filesystem_lab::truncate_tx::truncate_file_to_blocks_journaled;
use support::CrashDevice;

fn setup() -> (CrashDevice, Superblock, Vec<u64>, Vec<PersistedDirectoryEntry>) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let blocks = (0..4).map(|_| allocator.allocate().unwrap()).collect::<Vec<_>>();
    let inodes = vec![
        PersistedInode { id: 1, kind: InodeKind::Directory, blocks: Vec::new() },
        PersistedInode { id: 2, kind: InodeKind::File, blocks: blocks.clone() },
    ];
    let entries = vec![PersistedDirectoryEntry {
        parent: 1,
        target: 2,
        name: "file".to_owned(),
    }];

    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(&mut device, &superblock, &inodes).unwrap();
    store_directory_table(&mut device, &superblock, &entries).unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, blocks, entries)
}

fn state_is_old(device: &mut CrashDevice, sb: &Superblock, blocks: &[u64]) -> bool {
    let allocator = load_allocator(device, sb).unwrap();
    let inodes = load_inode_table(device, sb).unwrap();
    let file = inodes.iter().find(|inode| inode.id == 2).unwrap();
    file.blocks == blocks && blocks.iter().all(|block| allocator.is_owned(*block).unwrap())
}

fn state_is_new(device: &mut CrashDevice, sb: &Superblock, blocks: &[u64]) -> bool {
    let allocator = load_allocator(device, sb).unwrap();
    let inodes = load_inode_table(device, sb).unwrap();
    let file = inodes.iter().find(|inode| inode.id == 2).unwrap();
    file.blocks == vec![blocks[0]]
        && allocator.is_owned(blocks[0]).unwrap()
        && blocks[1..]
            .iter()
            .all(|block| !allocator.is_owned(*block).unwrap())
}

#[test]
fn truncate_to_blocks_releases_exact_tail_and_checkpoints() {
    let (mut device, superblock, blocks, entries) = setup();
    let (released, report) =
        truncate_file_to_blocks_journaled(&mut device, &superblock, 2, 1).unwrap();

    assert_eq!(released, blocks[1..]);
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 2);
    assert!(state_is_new(&mut device, &superblock, &blocks));
    assert_eq!(load_directory_table(&mut device, &superblock).unwrap(), entries);
    assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn truncate_to_blocks_rejects_growth_and_directory_and_noops_at_current_size() {
    let (mut device, superblock, blocks, _) = setup();

    let error = truncate_file_to_blocks_journaled(&mut device, &superblock, 2, 5).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    let error = truncate_file_to_blocks_journaled(&mut device, &superblock, 1, 0).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

    let (released, report) =
        truncate_file_to_blocks_journaled(&mut device, &superblock, 2, 4).unwrap();
    assert!(released.is_empty());
    assert_eq!(report, RecoveryReport::default());
    assert!(state_is_old(&mut device, &superblock, &blocks));
    check_device(&mut device).unwrap();
}

#[test]
fn every_multi_block_truncate_mutation_crash_point_recovers_to_old_or_new_state() {
    let (mut probe, superblock, blocks, _) = setup();
    probe.arm(None);
    truncate_file_to_blocks_journaled(&mut probe, &superblock, 2, 1).unwrap();
    let mutation_operations = probe.operations();
    assert!(mutation_operations >= 6);
    assert!(state_is_new(&mut probe, &superblock, &blocks));
    check_device(&mut probe).unwrap();

    for crash_at in 0..mutation_operations {
        let (mut device, superblock, blocks, _) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            truncate_file_to_blocks_journaled(&mut device, &superblock, 2, 1)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();

        let raw_old = state_is_old(&mut device, &superblock, &blocks);
        let raw_new = state_is_new(&mut device, &superblock, &blocks);
        if raw_old || raw_new {
            check_device(&mut device).unwrap();
        } else {
            assert!(check_device(&mut device).is_err());
        }

        recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
        assert!(state_is_old(&mut device, &superblock, &blocks)
            || state_is_new(&mut device, &superblock, &blocks));
        check_device(&mut device).unwrap();
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }
}
