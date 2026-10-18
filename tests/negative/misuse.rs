//! Negative tests: every allocator must refuse misuse with a precise error and keep its
//! state intact. Pointers that were never handed out are only ever compared by address;
//! nothing is read through them.

use qs_memory::{
    AllocError, Allocator, Block, BuddyAllocator, FreeListAllocator, PoolAllocator,
    SizeClassAllocator, SlabAllocator, SystemAllocator, Tracking,
};

fn foreign() -> Block {
    // SAFETY: the pointer refers to a static byte and is only passed to refusing calls,
    // which compare addresses and never read the memory.
    unsafe { Block::from_raw_parts((FOREIGN.0.as_ptr() as *mut u8).wrapping_add(1), 8, 8) }
}

#[repr(align(64))]
struct Aligned([u8; 64]);

static FOREIGN: Aligned = Aligned([0; 64]);

fn check_refusals<A: Allocator>(a: &A, live: Block) {
    // SAFETY: each call below is a deliberate misuse that must be refused before any
    // access; `live` is the one valid block and is released last, exactly once.
    unsafe {
        assert_eq!(a.deallocate(Block::from_raw_parts(core::ptr::null_mut(), 8, 8)), Err(AllocError::InvalidArgument));
        assert!(a.deallocate(foreign()).is_err());
        assert!(a.deallocate(Block::from_raw_parts(live.ptr().add(1), live.size(), live.align())).is_err());
        a.deallocate(live).unwrap();
    }
}

#[test]
fn system_refuses_null_foreign_and_double_release() {
    let sys = SystemAllocator::new();
    let b = sys.allocate(64, 16).unwrap();
    check_refusals(&sys, b);
    assert_eq!(sys.stats().live_allocations(), 0);
}

#[test]
fn pool_refuses_foreign_interior_and_double_release() {
    let pool = PoolAllocator::create(64, 16, 4).unwrap();
    let b = pool.allocate(64, 16).unwrap();
    check_refusals(&pool, b);
    assert_eq!(pool.slots_in_use(), 0);
}

#[test]
fn freelist_refuses_foreign_interior_and_double_release() {
    let heap = FreeListAllocator::create(8192).unwrap();
    let b = heap.allocate(128, 16).unwrap();
    check_refusals(&heap, b);
    heap.verify().unwrap();
}

#[test]
fn buddy_refuses_foreign_interior_and_double_release() {
    let heap = BuddyAllocator::create(8192).unwrap();
    let b = heap.allocate(128, 16).unwrap();
    check_refusals(&heap, b);
    heap.verify().unwrap();
}

#[test]
fn slab_refuses_foreign_interior_and_double_release() {
    let slab = SlabAllocator::create(64, 16).unwrap();
    let b = slab.allocate(64, 16).unwrap();
    check_refusals(&slab, b);
    assert_eq!(slab.objects_in_use(), 0);
}

#[test]
fn size_class_refuses_foreign_and_double_release() {
    let sc = SizeClassAllocator::create(4).unwrap();
    let b = sc.allocate(40, 16).unwrap();
    // SAFETY: deliberate misuse; refused before any access.
    unsafe {
        assert!(sc.deallocate(foreign()).is_err());
        sc.deallocate(b).unwrap();
        assert_eq!(sc.deallocate(b), Err(AllocError::DoubleFree));
    }
}

#[test]
fn tracking_refuses_frees_it_never_recorded() {
    let sys = SystemAllocator::new();
    let tracked = Tracking::new(&sys, 16).unwrap();
    // SAFETY: the foreign block was never recorded, so the decorator refuses it.
    unsafe { assert_eq!(tracked.deallocate(foreign()), Err(AllocError::InvalidFree)) };
    let b = tracked.allocate(16, 8).unwrap();
    // SAFETY: released once through the same decorator.
    unsafe {
        assert_eq!(tracked.deallocate(Block::from_raw_parts(b.ptr(), 15, 8)), Err(AllocError::InvalidArgument));
        tracked.deallocate(b).unwrap();
    }
}

#[test]
fn zero_size_requests_are_always_empty_and_harmless() {
    let sys = SystemAllocator::new();
    let pool = PoolAllocator::create(16, 8, 1).unwrap();
    let heap = FreeListAllocator::create(256).unwrap();
    for empty in [sys.allocate(0, 8).unwrap(), pool.allocate(0, 8).unwrap(), heap.allocate(0, 8).unwrap()] {
        assert!(empty.is_empty());
        // SAFETY: an empty block is a no-op to release.
        unsafe {
            sys.deallocate(empty).unwrap();
        }
    }
    assert_eq!(pool.slots_in_use(), 0);
    heap.verify().unwrap();
}

#[test]
fn double_release_is_refused_where_occupancy_is_tracked() {
    let pool = PoolAllocator::create(64, 16, 2).unwrap();
    let b = pool.allocate(64, 16).unwrap();
    // SAFETY: the first release is valid; the second is the misuse under test and is refused.
    unsafe {
        pool.deallocate(b).unwrap();
        assert_eq!(pool.deallocate(b), Err(AllocError::DoubleFree));
    }
}
