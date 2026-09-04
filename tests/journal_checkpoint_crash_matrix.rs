mod support;

use filesystem_lab::block::{BlockDevice, BLOCK_SIZE};
use filesystem_lab::format::Superblock;
use filesystem_lab::journal::JournalLog;
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::{load_journal_image, store_journal_image};
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

fn prepared_committed_update() -> (CrashDevice, Superblock, u64, [u8; BLOCK_SIZE]) {
    let mut device = CrashDevice::new(32);
    let superblock = Superblock::with_journal_blocks(32, 2).unwrap();
    let home = superblock.reserved_blocks();
    let old = [0x11; BLOCK_SIZE];
    let new = [0x77; BLOCK_SIZE];

    device.write_block(home, &old).unwrap();
    device.flush().unwrap();

    let mut log = JournalLog::new();
    let txid = log.begin().unwrap();
    log.write(txid, home, new).unwrap();
    log.commit(txid).unwrap();
    store_journal_image(&mut device, superblock, log.entries()).unwrap();

    (device, superblock, home, new)
}

fn read_block(device: &mut CrashDevice, block: u64) -> [u8; BLOCK_SIZE] {
    let mut data = [0_u8; BLOCK_SIZE];
    device.read_block(block, &mut data).unwrap();
    data
}

#[test]
fn checkpoint_crash_matrix_preserves_recoverability_at_every_mutation_boundary() {
    let (prepared, superblock, home, expected) = prepared_committed_update();

    let mut probe = prepared.clone();
    probe.arm(None);
    let report = recover_journal_and_checkpoint(&mut probe, superblock).unwrap();
    assert_eq!(
        report,
        RecoveryReport {
            committed_transactions: 1,
            home_writes: 1,
        }
    );
    let mutation_count = probe.operations();
    assert!(mutation_count >= 4);
    probe.reboot();
    assert_eq!(read_block(&mut probe, home), expected);
    assert!(load_journal_image(&mut probe, superblock).unwrap().is_empty());

    for crash_at in 0..mutation_count {
        let mut device = prepared.clone();
        device.arm(Some(crash_at));
        assert!(recover_journal_and_checkpoint(&mut device, superblock).is_err());

        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(read_block(&mut device, home), expected, "crash_at={crash_at}");
        assert!(
            load_journal_image(&mut device, superblock).unwrap().is_empty(),
            "crash_at={crash_at}"
        );

        let second = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        assert_eq!(second, RecoveryReport::default(), "crash_at={crash_at}");
    }
}

#[test]
fn checkpointed_region_can_be_reused_for_a_new_transaction() {
    let (mut device, superblock, home, first) = prepared_committed_update();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    assert_eq!(read_block(&mut device, home), first);
    assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());

    let second = [0x99; BLOCK_SIZE];
    let mut log = JournalLog::new();
    let txid = log.begin().unwrap();
    log.write(txid, home, second).unwrap();
    log.commit(txid).unwrap();
    store_journal_image(&mut device, superblock, log.entries()).unwrap();

    let report = recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    assert_eq!(report.committed_transactions, 1);
    assert_eq!(report.home_writes, 1);
    assert_eq!(read_block(&mut device, home), second);
    assert!(load_journal_image(&mut device, superblock).unwrap().is_empty());
}
