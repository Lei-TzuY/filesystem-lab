mod support;

use std::io;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::{load_directory_table, store_directory_table};
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::hard_link_tx::hard_link_symlink_journaled;
use filesystem_lab::hard_unlink_tx::unlink_nonfinal_symlink_link_journaled;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::symlink::{create_symlink_journaled, read_symlink};
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 6;

fn root() -> PersistedInode {
    PersistedInode {
        id: 1,
        kind: InodeKind::Directory,
        blocks: Vec::new(),
    }
}

fn entry(target: u64, name: &str) -> PersistedDirectoryEntry {
    PersistedDirectoryEntry {
        parent: 1,
        target,
        name: name.to_owned(),
    }
}

fn setup() -> (CrashDevice, Superblock, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(&mut device, &superblock, &[root()]).unwrap();
    store_directory_table(&mut device, &superblock, &[]).unwrap();
    let (symlink, _) = create_symlink_journaled(
        &mut device,
        &superblock,
        1,
        "original",
        "../opaque/target",
    )
    .unwrap();
    hard_link_symlink_journaled(&mut device, &superblock, 1, "alias", symlink).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock, symlink)
}

#[test]
fn every_nonfinal_symlink_unlink_crash_point_is_old_or_recoverable_new_namespace() {
    let (mut probe, superblock, symlink) = setup();
    let old_allocator = load_allocator(&mut probe, &superblock).unwrap();
    let old_inodes = load_inode_table(&mut probe, &superblock).unwrap();
    let old_namespace = vec![entry(symlink, "original"), entry(symlink, "alias")];
    let new_namespace = vec![entry(symlink, "original")];

    probe.arm(None);
    unlink_nonfinal_symlink_link_journaled(&mut probe, &superblock, 1, "alias").unwrap();
    let operations = probe.operations();
    assert!(operations >= 3);
    assert_eq!(load_directory_table(&mut probe, &superblock).unwrap(), new_namespace);
    assert_eq!(load_allocator(&mut probe, &superblock).unwrap(), old_allocator);
    assert_eq!(load_inode_table(&mut probe, &superblock).unwrap(), old_inodes);
    assert_eq!(read_symlink(&mut probe, &superblock, symlink).unwrap(), "../opaque/target");
    check_device(&mut probe).unwrap();

    for crash_at in 0..operations {
        let (mut device, superblock, symlink) = setup();
        let allocator = load_allocator(&mut device, &superblock).unwrap();
        let inodes = load_inode_table(&mut device, &superblock).unwrap();
        device.arm(Some(crash_at));
        assert_eq!(
            unlink_nonfinal_symlink_link_journaled(&mut device, &superblock, 1, "alias")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
        device.reboot();

        let before = load_directory_table(&mut device, &superblock).unwrap();
        assert!(before == old_namespace || before == new_namespace);
        assert_eq!(load_allocator(&mut device, &superblock).unwrap(), allocator);
        assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
        assert_eq!(read_symlink(&mut device, &superblock, symlink).unwrap(), "../opaque/target");
        check_device(&mut device).unwrap();

        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        let recovered = load_directory_table(&mut device, &superblock).unwrap();
        if report.committed_transactions == 0 {
            assert!(recovered == old_namespace || recovered == new_namespace);
        } else {
            assert_eq!(recovered, new_namespace);
        }
        assert_eq!(load_allocator(&mut device, &superblock).unwrap(), allocator);
        assert_eq!(load_inode_table(&mut device, &superblock).unwrap(), inodes);
        check_device(&mut device).unwrap();

        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second.committed_transactions, 0);
        assert_eq!(load_directory_table(&mut device, &superblock).unwrap(), recovered);
    }
}

#[test]
fn nonfinal_symlink_unlink_rejects_final_reference_and_wrong_kind() {
    let (mut device, superblock, symlink) = setup();
    unlink_nonfinal_symlink_link_journaled(&mut device, &superblock, 1, "alias").unwrap();
    assert_eq!(
        unlink_nonfinal_symlink_link_journaled(&mut device, &superblock, 1, "original")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        unlink_nonfinal_symlink_link_journaled(&mut device, &superblock, 1, "missing")
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(read_symlink(&mut device, &superblock, symlink).unwrap(), "../opaque/target");
    check_device(&mut device).unwrap();
}
