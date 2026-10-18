//! Integration tests: the crate exercised only through its public API, as a downstream
//! user would see it.

use qs_memory::*;

#[test]
fn arena_pool_and_freelist_through_the_trait() {
    fn exercise<A: Allocator>(a: &A) {
        let b = a.allocate(48, 16).unwrap();
        assert_eq!(b.ptr() as usize & 15, 0);
        // SAFETY: `b` came from `a` and is released once.
        if a.capabilities().contains(Capabilities::INDIVIDUAL_FREE) {
            unsafe { a.deallocate(b).unwrap() };
        }
    }

    let arena = Arena::new(4096).unwrap();
    exercise(&arena);
    let pool = PoolAllocator::create(64, 16, 8).unwrap();
    exercise(&pool);
    let heap = FreeListAllocator::create(4096).unwrap();
    exercise(&heap);
    heap.verify().unwrap();
    let sys = SystemAllocator::new();
    exercise(&sys);
    assert_eq!(sys.stats().live_allocations(), 0);
}

#[test]
fn checkpoints_and_scopes() {
    let arena = Arena::new(1024).unwrap();
    arena.allocate(16, 8).unwrap();
    {
        let _scope = ArenaScope::new(&arena);
        arena.allocate(100, 8).unwrap();
    }
    assert_eq!(arena.used(), 16);
}

#[test]
fn containers_on_a_tracked_allocator() {
    let sys = SystemAllocator::new();
    let tracked = Tracking::new(&sys, 256).unwrap();
    {
        let mut v: Vector<u64, _> = Vector::new(&tracked).unwrap();
        for i in 0..200 {
            v.push(i).unwrap();
        }
        assert_eq!(v.iter().sum::<u64>(), 19900);
        let mut m: HashMap<u32, u32, _> = HashMap::new(&tracked).unwrap();
        m.insert(7, 49).unwrap();
        assert_eq!(m.get(&7), Some(&49));
        assert!(tracked.live_count() >= 2);
    }
    assert_eq!(tracked.live_count(), 0);
    assert_eq!(sys.stats().live_allocations(), 0);
}

#[test]
fn owned_block_releases_on_drop_and_errors_are_values() {
    let pool = PoolAllocator::create(16, 8, 1).unwrap();
    {
        let owned = make_owned(&pool, 16, 8).unwrap();
        assert_eq!(pool.slots_in_use(), 1);
        drop(owned);
    }
    assert_eq!(pool.slots_in_use(), 0);
    assert_eq!(pool.allocate(16, 3).unwrap_err(), AllocError::InvalidAlignment);
}

#[test]
fn c_visible_error_codes_match_the_header() {
    assert_eq!(AllocError::DoubleFree.code(), 7);
    assert_eq!(AllocError::NotOwned.code(), 13);
    assert_eq!(qs_memory::config::DEFAULT_ALIGNMENT, 16);
}
