//! Tests for the live-block checks: zeroing over the free list, buddy and slab refuses misuse without
//! touching memory, and each heap still verifies afterwards.

use qs_memory::*;

#[test]
fn zeroing_over_freelist_refuses_interior_pointers() {
    let heap = FreeListAllocator::create(8192).unwrap();
    let zeroing = Zeroing::new(&heap);
    let block = zeroing.allocate(64, 16).unwrap();
    // SAFETY: the block is live and 64 bytes long.
    unsafe { block.as_mut_slice().fill(0x33) };
    // SAFETY: an interior pointer that the decorator must refuse without writing.
    let interior = unsafe { Block::from_raw_parts(block.ptr().wrapping_add(8), 8, 8) };
    // SAFETY: deliberate misuse.
    assert!(unsafe { zeroing.deallocate(interior) }.is_err());
    // SAFETY: the block is live and 64 bytes long.
    assert!(unsafe { block.as_mut_slice() }.iter().all(|&b| b == 0x33));
    // SAFETY: a live block released once.
    unsafe { zeroing.deallocate(block).unwrap() };
    heap.verify().unwrap();
}

#[test]
fn zeroing_over_buddy_refuses_interior_pointers() {
    let heap = BuddyAllocator::create(8192).unwrap();
    let zeroing = Zeroing::new(&heap);
    let block = zeroing.allocate(64, 16).unwrap();
    // SAFETY: the block is live and 64 bytes long.
    unsafe { block.as_mut_slice().fill(0x44) };
    // SAFETY: an interior pointer the decorator must refuse without writing.
    let interior = unsafe { Block::from_raw_parts(block.ptr().wrapping_add(8), 8, 8) };
    // SAFETY: deliberate misuse.
    assert!(unsafe { zeroing.deallocate(interior) }.is_err());
    // SAFETY: the block is live and 64 bytes long.
    assert!(unsafe { block.as_mut_slice() }.iter().all(|&b| b == 0x44));
    // SAFETY: a live block released once.
    unsafe { zeroing.deallocate(block).unwrap() };
    heap.verify().unwrap();
}

#[test]
fn zeroing_over_slab_refuses_interior_pointers() {
    let slab = SlabAllocator::create(64, 16).unwrap();
    let zeroing = Zeroing::new(&slab);
    let block = zeroing.allocate(64, 16).unwrap();
    // SAFETY: the block is live and 64 bytes long.
    unsafe { block.as_mut_slice().fill(0x55) };
    // SAFETY: an interior pointer the decorator must refuse without writing.
    let interior = unsafe { Block::from_raw_parts(block.ptr().wrapping_add(8), 8, 8) };
    // SAFETY: deliberate misuse.
    assert!(unsafe { zeroing.deallocate(interior) }.is_err());
    // SAFETY: the block is live and 64 bytes long.
    assert!(unsafe { block.as_mut_slice() }.iter().all(|&b| b == 0x55));
    // SAFETY: a live block released once.
    unsafe { zeroing.deallocate(block).unwrap() };
    assert_eq!(slab.objects_in_use(), 0);
}
