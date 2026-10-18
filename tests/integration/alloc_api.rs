//! Pool, free-list and buddy behaviour through the public API.

use qs_memory::{AllocError, Allocator, BuddyAllocator, FreeListAllocator, PoolAllocator};

#[test]
fn pool_hands_out_every_slot_then_refuses() {
    let pool = PoolAllocator::create(48, 16, 4).unwrap();
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(pool.allocate(48, 16).unwrap());
    }
    assert_eq!(pool.slots_free(), 0);
    assert_eq!(pool.allocate(48, 16).unwrap_err(), AllocError::CapacityExhausted);
    for b in held {
        // SAFETY: each block came from this pool and is released once.
        unsafe { pool.deallocate(b).unwrap() };
    }
    assert_eq!(pool.slots_in_use(), 0);
}

#[test]
fn pool_reports_slot_geometry() {
    let pool = PoolAllocator::create(20, 8, 10).unwrap();
    assert_eq!(pool.object_size(), 20);
    assert_eq!(pool.slot_count(), 10);
    assert!(pool.slot_size() >= 20);
    assert_eq!(pool.slot_size() % 8, 0);
}

#[test]
fn freelist_coalesces_and_verifies() {
    let heap = FreeListAllocator::create(8192).unwrap();
    let a = heap.allocate(100, 16).unwrap();
    let b = heap.allocate(100, 16).unwrap();
    let c = heap.allocate(100, 16).unwrap();
    // SAFETY: each block is released exactly once.
    unsafe {
        heap.deallocate(b).unwrap();
        heap.verify().unwrap();
        heap.deallocate(a).unwrap();
        heap.deallocate(c).unwrap();
    }
    let f = heap.fragmentation();
    assert_eq!(f.free_block_count, 1);
    assert_eq!(f.allocated_block_count, 0);
    heap.verify().unwrap();
}

#[test]
fn freelist_grows_in_place_when_neighbour_is_free() {
    let heap = FreeListAllocator::create(4096).unwrap();
    let x = heap.allocate(64, 16).unwrap();
    let y = heap.allocate(64, 16).unwrap();
    // SAFETY: `y` is released once; `x` is then resized while still live.
    unsafe {
        heap.deallocate(y).unwrap();
        let mut grown = x;
        heap.try_grow(&mut grown, 200).unwrap();
        assert_eq!(grown.ptr(), x.ptr());
        assert_eq!(grown.size(), 200);
        heap.deallocate(grown).unwrap();
    }
    heap.verify().unwrap();
}

#[test]
fn buddy_rounds_to_powers_of_two_and_merges_back() {
    let buddy = BuddyAllocator::create(4096).unwrap();
    let a = buddy.allocate(100, 1).unwrap();
    let s = buddy.structure();
    assert_eq!(s.allocated_blocks, 1);
    assert!(s.free_bytes < 4096);
    // SAFETY: released once.
    unsafe { buddy.deallocate(a).unwrap() };
    assert_eq!(buddy.structure().free_blocks, 1);
    buddy.verify().unwrap();
}

#[test]
fn buddy_refuses_double_free() {
    let buddy = BuddyAllocator::create(1024).unwrap();
    let a = buddy.allocate(64, 1).unwrap();
    // SAFETY: the second release is the misuse under test; the heap refuses it.
    unsafe {
        buddy.deallocate(a).unwrap();
        assert_eq!(buddy.deallocate(a), Err(AllocError::DoubleFree));
    }
}
