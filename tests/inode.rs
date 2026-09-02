use filesystem_lab::{
    allocation::BlockAllocator,
    inode::{InodeError, InodeKind, InodeTable},
};

#[test]
fn inode_ids_are_monotonic_and_kinds_are_preserved() {
    let mut table = InodeTable::new();
    let file = table.create(InodeKind::File).expect("create file inode");
    let directory = table
        .create(InodeKind::Directory)
        .expect("create directory inode");

    assert_eq!(file.get(), 1);
    assert_eq!(directory.get(), 2);
    assert_eq!(table.get(file).expect("file inode").kind(), InodeKind::File);
    assert_eq!(
        table.get(directory).expect("directory inode").kind(),
        InodeKind::Directory
    );
    assert_eq!(table.len(), 2);
}

#[test]
fn allocated_block_has_single_inode_owner() {
    let mut allocator = BlockAllocator::new(8, 2).expect("allocator");
    let block = allocator.allocate().expect("data block");
    let mut table = InodeTable::new();
    let first = table.create(InodeKind::File).expect("first inode");
    let second = table.create(InodeKind::File).expect("second inode");

    table
        .attach_block(first, block, &allocator)
        .expect("first owner attaches block");
    assert_eq!(
        table.attach_block(second, block, &allocator),
        Err(InodeError::BlockAlreadyOwned {
            block,
            owner: first,
        })
    );
    table.validate(&allocator).expect("ownership invariants");
}

#[test]
fn reserved_and_free_blocks_cannot_be_attached() {
    let allocator = BlockAllocator::new(8, 2).expect("allocator");
    let mut table = InodeTable::new();
    let inode = table.create(InodeKind::File).expect("inode");

    assert_eq!(
        table.attach_block(inode, 0, &allocator),
        Err(InodeError::ReservedBlock(0))
    );
    assert_eq!(
        table.attach_block(inode, 2, &allocator),
        Err(InodeError::BlockNotAllocated(2))
    );
}

#[test]
fn inode_removal_requires_detaching_blocks_first() {
    let mut allocator = BlockAllocator::new(8, 2).expect("allocator");
    let block = allocator.allocate().expect("block");
    let mut table = InodeTable::new();
    let inode = table.create(InodeKind::File).expect("inode");

    table
        .attach_block(inode, block, &allocator)
        .expect("attach block");
    assert_eq!(
        table.remove(inode),
        Err(InodeError::InodeStillOwnsBlocks(inode))
    );

    table.detach_block(inode, block).expect("detach block");
    allocator.free(block).expect("free block");
    let removed = table.remove(inode).expect("remove empty inode");
    assert_eq!(removed.id(), inode);
    assert!(table.is_empty());
    table.validate(&allocator).expect("final invariants");
    allocator.validate().expect("allocator invariants");
}

#[test]
fn validation_detects_free_block_still_referenced_by_inode() {
    let mut allocator = BlockAllocator::new(8, 2).expect("allocator");
    let block = allocator.allocate().expect("block");
    let mut table = InodeTable::new();
    let inode = table.create(InodeKind::File).expect("inode");
    table
        .attach_block(inode, block, &allocator)
        .expect("attach block");

    allocator.free(block).expect("simulate ownership violation");
    let error = table
        .validate(&allocator)
        .expect_err("free referenced block must fail validation");
    assert!(format!("{error}").contains("unallocated block"));
}
