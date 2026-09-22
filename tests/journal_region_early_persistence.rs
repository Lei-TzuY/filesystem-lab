mod support;

use filesystem_lab::block::BLOCK_SIZE;
use filesystem_lab::format::Superblock;
use filesystem_lab::journal::{JournalEntry, JournalLog};
use filesystem_lab::journal_region::{load_journal_image, store_journal_image};
use support::CrashDevice;

fn sample_entries(superblock: Superblock) -> Vec<JournalEntry> {
    let mut log = JournalLog::new();
    let txid = log.begin().unwrap();
    log.write(
        txid,
        superblock.reserved_blocks(),
        [0x6d; BLOCK_SIZE],
    )
    .unwrap();
    log.commit(txid).unwrap();
    log.entries().to_vec()
}

#[test]
fn write_through_publication_is_always_empty_or_complete_after_reboot() {
    let superblock = Superblock::with_journal_blocks(32, 2).unwrap();
    let entries = sample_entries(superblock);

    let mut probe = CrashDevice::new_write_through(32);
    probe.arm(None);
    store_journal_image(&mut probe, superblock, &entries).unwrap();
    let mutation_count = probe.operations();
    assert!(mutation_count >= 5);
    probe.reboot();
    assert_eq!(load_journal_image(&mut probe, superblock).unwrap(), entries);

    for crash_at in 0..mutation_count {
        let mut device = CrashDevice::new_write_through(32);
        device.arm(Some(crash_at));
        assert!(
            store_journal_image(&mut device, superblock, &entries).is_err(),
            "crash_at={crash_at}"
        );

        device.reboot();
        let recovered = load_journal_image(&mut device, superblock)
            .unwrap_or_else(|error| panic!("crash_at={crash_at}: {error}"));
        assert!(
            recovered.is_empty() || recovered == entries,
            "crash_at={crash_at}, recovered={recovered:?}"
        );
    }
}
