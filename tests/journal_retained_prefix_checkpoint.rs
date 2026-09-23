mod support;

use filesystem_lab::allocation_disk::{load_allocator, store_allocator};
use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::journal::JournalEntry;
use filesystem_lab::journal_checkpoint::{
    recover_journal_and_checkpoint, recover_retained_prefix_and_checkpoint,
    RetainedPrefixCheckpointReport,
};
use filesystem_lab::journal_region::{append_retained_journal_entries, load_journal_image};
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn committed_tx(txid: u64, block: u64, fill: u8) -> Vec<JournalEntry> {
    vec![
        JournalEntry::Begin { txid },
        JournalEntry::Write {
            txid,
            block,
            data: Box::new([fill; BLOCK_SIZE]),
        },
        JournalEntry::Commit { txid },
    ]
}

fn read_block(device: &mut CrashDevice, block: u64) -> [u8; BLOCK_SIZE] {
    let mut data = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut data).unwrap();
    data
}

#[test]
fn prefix_checkpoint_reclaims_retained_bank_capacity() {
    let mut device = CrashDevice::new(64);
    let superblock = format_device_with_journal_blocks(&mut device, 8).unwrap();
    let first_home = superblock.reserved_blocks();
    let second_home = first_home + 1;
    let third_home = first_home + 2;
    let first = committed_tx(1, first_home, 0x11);
    let second = committed_tx(2, second_home, 0x22);
    let third = committed_tx(3, third_home, 0x33);

    append_retained_journal_entries(&mut device, superblock, &first).unwrap();
    append_retained_journal_entries(&mut device, superblock, &second).unwrap();

    let error = append_retained_journal_entries(&mut device, superblock, &third).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);

    let report =
        recover_retained_prefix_and_checkpoint(&mut device, superblock, 1).unwrap();
    assert_eq!(
        report,
        RetainedPrefixCheckpointReport {
            replayed: RecoveryReport {
                committed_transactions: 1,
                home_writes: 1,
            },
            remaining_transactions: 1,
        }
    );
    assert_eq!(read_block(&mut device, first_home), [0x11; BLOCK_SIZE]);
    assert_eq!(read_block(&mut device, second_home), [0; BLOCK_SIZE]);
    assert_eq!(load_journal_image(&mut device, superblock).unwrap(), second);

    append_retained_journal_entries(&mut device, superblock, &third).unwrap();
    let mut retained = second.clone();
    retained.extend(third.clone());
    assert_eq!(
        load_journal_image(&mut device, superblock).unwrap(),
        retained
    );

    let final_report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    assert_eq!(
        final_report,
        RecoveryReport {
            committed_transactions: 2,
            home_writes: 2,
        }
    );
    assert_eq!(read_block(&mut device, second_home), [0x22; BLOCK_SIZE]);
    assert_eq!(read_block(&mut device, third_home), [0x33; BLOCK_SIZE]);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
    check_device(&mut device).unwrap();
}

#[test]
fn write_through_prefix_checkpoint_crash_matrix_converges() {
    let mut prepared = CrashDevice::new(64);
    prepared.write_through = true;
    let superblock = format_device_with_journal_blocks(&mut prepared, 8).unwrap();
    let first_home = superblock.reserved_blocks();
    let second_home = first_home + 1;
    let first = committed_tx(7, first_home, 0x47);
    let second = committed_tx(8, second_home, 0x88);

    append_retained_journal_entries(&mut prepared, superblock, &first).unwrap();
    append_retained_journal_entries(&mut prepared, superblock, &second).unwrap();

    let mut probe = prepared.clone();
    probe.arm(None);
    recover_retained_prefix_and_checkpoint(&mut probe, superblock, 1).unwrap();
    let mutation_count = probe.operations();
    assert!(mutation_count >= 4);

    for crash_at in 0..mutation_count {
        let mut device = prepared.clone();
        device.arm(Some(crash_at));
        assert!(
            recover_retained_prefix_and_checkpoint(&mut device, superblock, 1).is_err(),
            "crash_at={crash_at}"
        );

        device.reboot();
        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert!(
            report.committed_transactions == 1 || report.committed_transactions == 2,
            "crash_at={crash_at}, report={report:?}"
        );
        assert_eq!(
            read_block(&mut device, first_home),
            [0x47; BLOCK_SIZE],
            "crash_at={crash_at}"
        );
        assert_eq!(
            read_block(&mut device, second_home),
            [0x88; BLOCK_SIZE],
            "crash_at={crash_at}"
        );
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

#[test]
fn semantic_bad_prefix_is_rejected_before_any_mutation() {
    let mut device = CrashDevice::new(64);
    let superblock = format_device_with_journal_blocks(&mut device, 8).unwrap();

    let mut desired = device.clone();
    let mut allocator = load_allocator(&mut desired, &superblock).unwrap();
    let orphan = allocator.allocate().unwrap();
    store_allocator(&mut desired, &superblock, &allocator).unwrap();
    let mut allocation_image = [0_u8; BLOCK_SIZE];
    desired
        .read_block(superblock.allocation_start, &mut allocation_image)
        .unwrap();

    let bad = vec![
        JournalEntry::Begin { txid: 31 },
        JournalEntry::Write {
            txid: 31,
            block: superblock.allocation_start,
            data: Box::new(allocation_image),
        },
        JournalEntry::Commit { txid: 31 },
    ];
    append_retained_journal_entries(&mut device, superblock, &bad).unwrap();
    let retained_before = load_journal_image(&mut device, superblock).unwrap();

    device.arm(None);
    let error =
        recover_retained_prefix_and_checkpoint(&mut device, superblock, 1).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains(&format!("allocated block {orphan} has no inode owner")));
    assert_eq!(device.operations(), 0);
    assert_eq!(
        load_journal_image(&mut device, superblock).unwrap(),
        retained_before
    );
    assert_eq!(
        load_allocator(&mut device, &superblock)
            .unwrap()
            .allocated_blocks(),
        0
    );
}
