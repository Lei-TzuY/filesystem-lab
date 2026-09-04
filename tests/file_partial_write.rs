mod support;

use std::io;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::file_data::{read_file_block, write_file_block_range_journaled};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let block = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();
    store_inode_table(&mut device, &superblock, &[PersistedInode { id: 1, kind: InodeKind::Directory, blocks: vec![] }, PersistedInode { id: 2, kind: InodeKind::File, blocks: vec![block] }]).unwrap();
    store_directory_table(&mut device, &superblock, &[PersistedDirectoryEntry { parent: 1, target: 2, name: "file".into() }]).unwrap();
    device.write_block(block, &[0x11; BLOCK_SIZE]).unwrap();
    device.flush().unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn partial_write_preserves_unmodified_bytes_and_reuses_journal() {
    let (mut device, superblock) = setup();
    let report = write_file_block_range_journaled(&mut device, &superblock, 2, 0, 100, &[1, 2, 3, 4]).unwrap();
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 1);
    let image = read_file_block(&mut device, &superblock, 2, 0).unwrap();
    assert_eq!(&image[100..104], &[1, 2, 3, 4]);
    assert!(image[..100].iter().all(|byte| *byte == 0x11));
    assert!(image[104..].iter().all(|byte| *byte == 0x11));
    assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn invalid_partial_ranges_do_not_mutate_data() {
    let (mut device, superblock) = setup();
    for (offset, data) in [(0, &[][..]), (BLOCK_SIZE, &[1][..]), (BLOCK_SIZE - 1, &[1, 2][..])] {
        assert_eq!(write_file_block_range_journaled(&mut device, &superblock, 2, 0, offset, data).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }
    assert_eq!(read_file_block(&mut device, &superblock, 2, 0).unwrap(), [0x11; BLOCK_SIZE]);
}

#[test]
fn every_partial_write_crash_point_recovers_whole_old_or_new_block() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    write_file_block_range_journaled(&mut probe, &superblock, 2, 0, 200, &[0xaa; 17]).unwrap();
    let operations = probe.operations();
    let mut expected = [0x11; BLOCK_SIZE];
    expected[200..217].fill(0xaa);

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(write_file_block_range_journaled(&mut device, &superblock, 2, 0, 200, &[0xaa; 17]).unwrap_err().kind(), io::ErrorKind::Other);
        device.reboot();
        let raw = read_file_block(&mut device, &superblock, 2, 0).unwrap();
        assert!(raw == [0x11; BLOCK_SIZE] || raw == expected);
        check_device(&mut device).unwrap();
        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        if report.committed_transactions == 0 { assert_eq!(read_file_block(&mut device, &superblock, 2, 0).unwrap(), [0x11; BLOCK_SIZE]); } else { assert_eq!(read_file_block(&mut device, &superblock, 2, 0).unwrap(), expected); }
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
        assert_eq!(recover_journal_and_checkpoint(&mut device, superblock).unwrap(), RecoveryReport::default());
    }
}
