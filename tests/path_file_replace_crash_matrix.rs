mod support;

use std::collections::HashSet;

use filesystem_lab::allocation_disk::load_allocator;
use filesystem_lab::block::BLOCK_SIZE;
use filesystem_lab::format::Superblock;
use filesystem_lab::format_geometry::format_device_with_journal_blocks;
use filesystem_lab::fsck::check_device;
use filesystem_lab::inode::InodeKind;
use filesystem_lab::inode_codec::PersistedInode;
use filesystem_lab::inode_table::{load_inode_table, store_inode_table};
use filesystem_lab::journal_checkpoint::recover_journal_and_checkpoint;
use filesystem_lab::journal_region::load_journal_image;
use filesystem_lab::path_create::create_file_with_blocks_at_path_journaled;
use filesystem_lab::path_file_read::read_file_blocks_at_path;
use filesystem_lab::path_file_write::replace_file_at_path_journaled;
use filesystem_lab::recovery::RecoveryReport;
use support::CrashDevice;

const JOURNAL_BLOCKS: u64 = 10;
const ORIGINAL: [[u8; BLOCK_SIZE]; 2] = [[0x11; BLOCK_SIZE], [0x22; BLOCK_SIZE]];
const REPLACEMENT: [[u8; BLOCK_SIZE]; 3] =
    [[0xa1; BLOCK_SIZE], [0xb2; BLOCK_SIZE], [0xc3; BLOCK_SIZE]];

fn setup() -> (CrashDevice, Superblock) {
    let mut device = CrashDevice::new(128);
    let superblock = format_device_with_journal_blocks(&mut device, JOURNAL_BLOCKS).unwrap();
    store_inode_table(
        &mut device,
        &superblock,
        &[PersistedInode {
            id: 1,
            kind: InodeKind::Directory,
            blocks: Vec::new(),
        }],
    )
    .unwrap();
    create_file_with_blocks_at_path_journaled(&mut device, &superblock, "/file", &ORIGINAL)
        .unwrap();
    recover_journal_and_checkpoint(&mut device, superblock).unwrap();
    check_device(&mut device).unwrap();
    (device, superblock)
}

fn assert_unique_ownership(device: &mut CrashDevice, superblock: &Superblock) {
    let allocator = load_allocator(device, superblock).unwrap();
    allocator.validate().unwrap();
    let inodes = load_inode_table(device, superblock).unwrap();
    let mut seen = HashSet::new();
    for inode in &inodes {
        for block in &inode.blocks {
            assert!(
                seen.insert(*block),
                "duplicate physical block reference {block}"
            );
            assert!(allocator.is_owned(*block).unwrap());
        }
    }
}

#[test]
fn whole_file_replacement_is_old_or_new_across_every_crash_boundary() {
    let (mut probe, superblock) = setup();
    probe.arm(None);
    replace_file_at_path_journaled(&mut probe, &superblock, "/file", &REPLACEMENT).unwrap();
    let operations = probe.operations();
    assert!(operations > 0);

    let old_bytes = ORIGINAL.concat();
    let new_bytes = REPLACEMENT.concat();
    let mut interrupted = 0;
    let mut recovered_old = 0;
    let mut recovered_new = 0;

    for crash_at in 0..operations {
        let (mut device, superblock) = setup();
        device.arm(Some(crash_at));
        if replace_file_at_path_journaled(&mut device, &superblock, "/file", &REPLACEMENT).is_ok() {
            continue;
        }
        interrupted += 1;

        device.reboot();
        recover_journal_and_checkpoint(&mut device, superblock).unwrap();
        let observed = read_file_blocks_at_path(&mut device, &superblock, "/file").unwrap();
        if observed == old_bytes {
            recovered_old += 1;
        } else if observed == new_bytes {
            recovered_new += 1;
        } else {
            panic!("crash recovery exposed neither complete old nor complete new file image");
        }

        assert_unique_ownership(&mut device, &superblock);
        check_device(&mut device).unwrap();
        assert!(load_journal_image(&mut device, superblock)
            .unwrap()
            .is_empty());
        assert_eq!(
            recover_journal_and_checkpoint(&mut device, superblock).unwrap(),
            RecoveryReport::default()
        );
    }

    assert!(interrupted > 0);
    assert!(recovered_old > 0);
    assert!(recovered_new > 0);
}
