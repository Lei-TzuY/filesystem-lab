mod support;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::Superblock;
use filesystem_lab::journal::JournalEntry;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::{
    append_retained_journal_entries, load_journal_image, store_journal_image,
};
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
fn retained_v3_log_replays_multiple_transactions_without_intermediate_checkpoint() {
    let superblock = Superblock::with_journal_blocks(64, 8).unwrap();
    let first_home = superblock.reserved_blocks();
    let second_home = first_home + 1;
    let first = committed_tx(1, first_home, 0x31);
    let second = committed_tx(2, second_home, 0x72);
    let mut combined = first.clone();
    combined.extend(second.clone());

    let mut device = CrashDevice::new(64);

    append_retained_journal_entries(&mut device, superblock, &first).unwrap();
    assert_eq!(load_journal_image(&mut device, superblock).unwrap(), first);

    append_retained_journal_entries(&mut device, superblock, &second).unwrap();
    assert_eq!(
        load_journal_image(&mut device, superblock).unwrap(),
        combined
    );

    let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    assert_eq!(report.committed_transactions, 2);
    assert_eq!(report.home_writes, 2);
    assert_eq!(read_block(&mut device, first_home), [0x31; BLOCK_SIZE]);
    assert_eq!(read_block(&mut device, second_home), [0x72; BLOCK_SIZE]);
    assert!(load_journal_image(&mut device, superblock)
        .unwrap()
        .is_empty());
}

#[test]
fn write_through_second_append_exposes_only_old_or_new_complete_snapshot() {
    let superblock = Superblock::with_journal_blocks(64, 8).unwrap();
    let first_home = superblock.reserved_blocks();
    let second_home = first_home + 1;
    let first = committed_tx(7, first_home, 0x41);
    let second = committed_tx(8, second_home, 0x82);
    let mut combined = first.clone();
    combined.extend(second.clone());

    let mut prepared = CrashDevice::new(64);
    prepared.write_through = true;
    append_retained_journal_entries(&mut prepared, superblock, &first).unwrap();

    let mut probe = prepared.clone();
    probe.arm(None);
    append_retained_journal_entries(&mut probe, superblock, &second).unwrap();
    let mutation_count = probe.operations();
    assert!(mutation_count >= 4);

    for crash_at in 0..mutation_count {
        let mut device = prepared.clone();
        device.arm(Some(crash_at));
        assert!(
            append_retained_journal_entries(&mut device, superblock, &second).is_err(),
            "crash_at={crash_at}"
        );

        device.reboot();
        let visible = load_journal_image(&mut device, superblock)
            .unwrap_or_else(|error| panic!("crash_at={crash_at}: {error}"));
        assert!(
            visible == first || visible == combined,
            "crash_at={crash_at}, visible={visible:?}"
        );

        let expected_transactions = if visible == first { 1 } else { 2 };
        let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(
            report.committed_transactions, expected_transactions,
            "crash_at={crash_at}"
        );
        assert_eq!(read_block(&mut device, first_home), [0x41; BLOCK_SIZE]);
        if expected_transactions == 2 {
            assert_eq!(read_block(&mut device, second_home), [0x82; BLOCK_SIZE]);
        } else {
            assert_eq!(read_block(&mut device, second_home), [0; BLOCK_SIZE]);
        }
        assert!(
            load_journal_image(&mut device, superblock)
                .unwrap()
                .is_empty(),
            "crash_at={crash_at}"
        );
    }
}

#[test]
fn active_v2_log_must_checkpoint_before_retained_mode() {
    let superblock = Superblock::with_journal_blocks(64, 8).unwrap();
    let first_home = superblock.reserved_blocks();
    let second_home = first_home + 1;
    let first = committed_tx(11, first_home, 0x19);
    let second = committed_tx(12, second_home, 0x29);
    let mut device = CrashDevice::new(64);

    store_journal_image(&mut device, superblock, &first).unwrap();

    let error = append_retained_journal_entries(&mut device, superblock, &second).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    assert_eq!(load_journal_image(&mut device, superblock).unwrap(), first);
}
