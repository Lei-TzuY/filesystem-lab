use filesystem_lab::allocation::{AllocationError, BlockAllocator};

#[test]
fn reserved_prefix_is_never_allocated() {
    let mut allocator = BlockAllocator::new(8, 1).unwrap();

    let first = allocator.allocate().unwrap();
    assert_eq!(first, 1);
    assert!(allocator.is_owned(0).unwrap());
    assert!(allocator.is_owned(1).unwrap());
    allocator.validate().unwrap();
}

#[test]
fn allocator_never_double_owns_a_live_block() {
    let mut allocator = BlockAllocator::new(6, 1).unwrap();

    let a = allocator.allocate().unwrap();
    let b = allocator.allocate().unwrap();
    let c = allocator.allocate().unwrap();

    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_ne!(b, c);
    assert_eq!((a, b, c), (1, 2, 3));
    allocator.validate().unwrap();
}

#[test]
fn free_releases_exactly_one_block_and_reuses_it_deterministically() {
    let mut allocator = BlockAllocator::new(5, 1).unwrap();
    let a = allocator.allocate().unwrap();
    let b = allocator.allocate().unwrap();

    assert_eq!(allocator.allocated_blocks(), 2);
    assert_eq!(allocator.free_blocks(), 2);

    allocator.free(a).unwrap();
    assert_eq!(allocator.allocated_blocks(), 1);
    assert_eq!(allocator.free_blocks(), 3);
    assert_eq!(allocator.allocate().unwrap(), a);
    assert!(allocator.is_owned(b).unwrap());
    allocator.validate().unwrap();
}

#[test]
fn accounting_holds_through_allocate_free_cycle() {
    let mut allocator = BlockAllocator::new(10, 2).unwrap();
    assert_eq!(allocator.total_blocks(), 10);
    assert_eq!(allocator.reserved_blocks(), 2);
    assert_eq!(allocator.allocated_blocks(), 0);
    assert_eq!(allocator.free_blocks(), 8);
    allocator.validate().unwrap();

    let mut allocated = Vec::new();
    while let Ok(block) = allocator.allocate() {
        allocated.push(block);
        allocator.validate().unwrap();
    }
    assert_eq!(allocated, (2..10).collect::<Vec<_>>());
    assert_eq!(allocator.free_blocks(), 0);
    assert_eq!(allocator.allocate(), Err(AllocationError::Exhausted));

    for block in allocated {
        allocator.free(block).unwrap();
        allocator.validate().unwrap();
    }
    assert_eq!(allocator.allocated_blocks(), 0);
    assert_eq!(allocator.free_blocks(), 8);
}

#[test]
fn reserved_out_of_range_and_double_free_are_rejected() {
    let mut allocator = BlockAllocator::new(4, 1).unwrap();

    assert_eq!(allocator.free(0), Err(AllocationError::ReservedBlock(0)));
    assert!(matches!(
        allocator.free(4),
        Err(AllocationError::OutOfRange {
            block: 4,
            total_blocks: 4
        })
    ));
    assert_eq!(allocator.free(1), Err(AllocationError::AlreadyFree(1)));

    let block = allocator.allocate().unwrap();
    allocator.free(block).unwrap();
    assert_eq!(
        allocator.free(block),
        Err(AllocationError::AlreadyFree(block))
    );
    allocator.validate().unwrap();
}

#[test]
fn invalid_reserved_layout_is_rejected() {
    assert!(matches!(
        BlockAllocator::new(2, 3),
        Err(AllocationError::InvalidLayout {
            total_blocks: 2,
            reserved_blocks: 3
        })
    ));
}
