mod support;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::directory_codec::PersistedDirectoryEntry;
use filesystem_lab::directory_table::store_directory_table;
use filesystem_lab::format::{format_device, Superblock};
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::recovery::RecoveryReport;
use filesystem_lab::truncate_tx::truncate_file_to_bytes_journaled;
use support::CrashDevice;

fn prepared_file() -> (CrashDevice, Superblock, u64, u64) {
    let mut device = CrashDevice::new(64);
    let superblock = format_device(&mut device).unwrap();

    let mut allocator = load_allocator(&mut device, &superblock).unwrap();
    let first = allocator.allocate().unwrap();
    let second = allocator.allocate().unwrap();
    store_allocator(&mut device, &superblock, &allocator).unwrap();

    device.write_block(first, &[0xa5; BLOCK_SIZE]).unwrap();
    device.write_block(second, &[0x5a; BLOCK_SIZE]).unwrap();
    device.flush().unwrap();

    let root = PersistedInode::new(1, InodeKind::Directory, Vec::new()).unwrap();
    let file = PersistedInode::new(2, InodeKind::File, vec![first, second]).unwrap();
    store_inode_table(&mut device, &superblock, &[root, file]).unwrap();
    store_directory_table(
        &mut device,
        &superblock,
        &[PersistedDirectoryEntry {
            parent: 1,
            target: 2,
            name: "file".to_owned(),
        }],
    )
    .unwrap();

    check_device(&mut device).unwrap();
    (device, superblock, first, second)
}

fn read_block(device: &mut CrashDevice, block: u64) -> [u8; BLOCK_SIZE] {
    let mut image = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut image).unwrap();
    image
}

#[test]
fn exact_byte_shrink_updates_eof_releases_suffix_and_zeros_tail() {
    let (mut device, superblock, first, second) = prepared_file();

    let (released, report) =
        truncate_file_to_bytes_journaled(&mut device, &superblock, 2, 3000).unwrap();

    assert_eq!(released, vec![second]);
    assert_eq!(report.committed_transactions, 1);
    let inodes = load_inode_table(&mut device, &superblock).unwrap();
    let file = inodes.iter().find(|inode| inode.id == 2).unwrap();
    assert_eq!(file.blocks, vec![first]);
    assert_eq!(file.byte_len, 3000);

    let allocator = load_allocator(&mut device, &superblock).unwrap();
    assert!(allocator.is_owned(first).unwrap());
    assert!(!allocator.is_owned(second).unwrap());

    let first_image = read_block(&mut device, first);
    assert!(first_image[..3000].iter().all(|byte| *byte == 0xa5));
    assert!(first_image[3000..].iter().all(|byte| *byte == 0));
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn byte_shrink_crash_matrix_recovers_old_or_complete_new_state() {
    let (prepared, superblock, first, second) = prepared_file();

    let mut probe = prepared.clone();
    probe.arm(None);
    truncate_file_to_bytes_journaled(&mut probe, &superblock, 2, 3000).unwrap();
    let mutation_count = probe.operations();
    assert!(mutation_count > 0);

    for crash_at in 0..mutation_count {
        let mut device = prepared.clone();
        device.arm(Some(crash_at));
        assert!(
            truncate_file_to_bytes_journaled(&mut device, &superblock, 2, 3000).is_err(),
            "crash_at={crash_at}"
        );

        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();

        let inodes = load_inode_table(&mut device, &superblock).unwrap();
        let file = inodes.iter().find(|inode| inode.id == 2).unwrap();
        let allocator = load_allocator(&mut device, &superblock).unwrap();

        match file.byte_len {
            8192 => {
                assert_eq!(file.blocks, vec![first, second], "crash_at={crash_at}");
                assert!(allocator.is_owned(first).unwrap(), "crash_at={crash_at}");
                assert!(allocator.is_owned(second).unwrap(), "crash_at={crash_at}");
                assert_eq!(read_block(&mut device, first), [0xa5; BLOCK_SIZE]);
                assert_eq!(read_block(&mut device, second), [0x5a; BLOCK_SIZE]);
            }
            3000 => {
                assert_eq!(file.blocks, vec![first], "crash_at={crash_at}");
                assert!(allocator.is_owned(first).unwrap(), "crash_at={crash_at}");
                assert!(!allocator.is_owned(second).unwrap(), "crash_at={crash_at}");
                let image = read_block(&mut device, first);
                assert!(image[..3000].iter().all(|byte| *byte == 0xa5));
                assert!(image[3000..].iter().all(|byte| *byte == 0));
            }
            other => panic!("crash_at={crash_at}: unexpected EOF {other}"),
        }

        assert!(
            load_journal_image(&mut device, superblock)
                .unwrap()
                .is_empty(),
            "crash_at={crash_at}"
        );
        check_device(&mut device).unwrap();
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default(),
            "crash_at={crash_at}"
        );
    }
}
