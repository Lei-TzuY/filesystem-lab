mod support;

use std::io;

use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::hard_link_tx::hard_link_file_journaled;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::store_inode_table;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use support::CrashDevice;

fn inode(id: u64, kind: InodeKind) -> PersistedInode {
    PersistedInode { id, kind, blocks: Vec::new() }
}

fn entry(parent: u64, target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry { parent, target, name: name.to_owned() }
}

fn old_namespace() -> Vec<PersistedDirectoryEntry> {
    vec![entry(1, 2, "original")]
}

fn new_namespace() -> Vec<PersistedDirectoryEntry> {
    vec![entry(1, 2, "original"), entry(1, 2, "alias")]
}

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[inode(1, InodeKind::Directory), inode(2, InodeKind::File)],
    )
    .unwrap();
    store_directory_table(&mut device, &superblock, &old_namespace()).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

#[test]
fn every_hard_link_mutation_crash_point_is_old_or_recoverable_new_namespace() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    hard_link_file_journaled(&mut probe, &superblock, 1, "alias", 2).unwrap();
    let operations = probe.operations();
    assert!(operations >= 3);
    assert_eq!(load_directory_table(&mut probe, &superblock).unwrap(), new_namespace());
    check_device(&mut probe).unwrap();

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        assert_eq!(
            hard_link_file_journaled(&mut device, &superblock, 1, "alias", 2)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
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
        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second.committed_transactions, 0);
        assert_eq!(load_directory_table(&mut device, &superblock).unwrap(), recovered);
    }
}

#[test]
fn rejects_directory_target_and_existing_destination_before_publication() {
    let (mut device, superblock) = setup();
    assert_eq!(
        hard_link_file_journaled(&mut device, &superblock, 1, "bad", 1)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        hard_link_file_journaled(&mut device, &superblock, 1, "original", 2)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(load_directory_table(&mut device, &superblock).unwrap(), old_namespace());
    check_device(&mut device).unwrap();
}
