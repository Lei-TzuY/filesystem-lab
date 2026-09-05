mod support;

use std::io;

use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::hard_unlink_tx::unlink_nonfinal_file_link_journaled;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use support::CrashDevice;

fn entry(name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry { parent: 1, target: 2, name: name.to_owned() }
}

fn old_namespace() -> Vec<PersistedDirectoryEntry> {
    vec![entry("original"), entry("alias")]
}

fn new_namespace() -> Vec<PersistedDirectoryEntry> {
    vec![entry("original")]
}

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(&mut device, &superblock, &[
        PersistedInode { id: 1, kind: InodeKind::Directory, blocks: Vec::new() },
        PersistedInode { id: 2, kind: InodeKind::File, blocks: Vec::new() },
    ]).unwrap();
    store_directory_table(&mut device, &superblock, &old_namespace()).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn every_nonfinal_unlink_crash_point_is_old_or_recoverable_new_namespace() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    unlink_nonfinal_file_link_journaled(&mut probe, &superblock, 1, "alias").unwrap();
    let operations = probe.operations();
    assert!(operations >= 3);
    assert_eq!(load_directory_table(&mut probe, &superblock).unwrap(), new_namespace());
    check_device(&mut probe).unwrap();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(unlink_nonfinal_file_link_journaled(&mut device, &superblock, 1, "alias").unwrap_err().kind(), io::ErrorKind::Other);
        device.reboot();
        let before = load_directory_table(&mut device, &superblock).unwrap();
        assert!(before == old_namespace() || before == new_namespace());
        check_device(&mut device).unwrap();
        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        let recovered = load_directory_table(&mut device, &superblock).unwrap();
        if report.committed_transactions == 0 {
            assert!(recovered == old_namespace() || recovered == new_namespace());
        } else {
            assert_eq!(recovered, new_namespace());
        }
        check_device(&mut device).unwrap();
        assert_eq!(recover_journal_and_checkpoint(&mut device, superblock).unwrap().committed_transactions, 0);
    }
}

#[test]
fn rejects_final_link_before_publication() {
    let (mut device, superblock) = setup();
    store_directory_table(&mut device, &superblock, &new_namespace()).unwrap();
    assert_eq!(unlink_nonfinal_file_link_journaled(&mut device, &superblock, 1, "original").unwrap_err().kind(), io::ErrorKind::InvalidInput);
    assert_eq!(load_directory_table(&mut device, &superblock).unwrap(), new_namespace());
    check_device(&mut device).unwrap();
}
