//! Decorators and containers through the public API, composed the way applications use them.

use qs_memory::{
    AllocError, Allocator, Budget, FaultInjecting, Fallback, Guarded, HashMap, OwnedPtr,
    PoolAllocator, RingBuffer, SmallVector, SystemAllocator, Synchronized, Tracking, Vector,
};

#[test]
fn guard_reports_clean_round_trip() {
    let sys = SystemAllocator::new();
    let guarded = Guarded::new(&sys);
    let b = guarded.allocate(64, 16).unwrap();
    assert_eq!(guarded.check(b), Ok(()));
    // SAFETY: released once through the guard that produced it.
    unsafe { guarded.deallocate(b).unwrap() };
    assert_eq!(guarded.report().deallocations, 1);
}

#[test]
fn budget_and_fault_injection_stack_on_top_of_each_other() {
    let sys = SystemAllocator::new();
    let budget = Budget::new(&sys, 256);
    let faults = FaultInjecting::new(&budget);
    faults.fail_call(2);
    let a = faults.allocate(100, 8).unwrap();
    assert_eq!(faults.allocate(8, 8).unwrap_err(), AllocError::OutOfMemory);
    // SAFETY: released once.
    unsafe { faults.deallocate(a).unwrap() };
    assert_eq!(budget.live_bytes(), 0);
}

#[test]
fn fallback_routes_small_requests_to_the_pool() {
    let pool = PoolAllocator::create(32, 16, 2).unwrap();
    let sys = SystemAllocator::new();
    let fb = Fallback::new(&pool, &sys);
    let a = fb.allocate(32, 16).unwrap();
    let b = fb.allocate(32, 16).unwrap();
    let c = fb.allocate(32, 16).unwrap();
    assert_eq!(pool.slots_in_use(), 2);
    assert_eq!(sys.stats().live_allocations(), 1);
    // SAFETY: each block came from `fb`, released once.
    unsafe {
        fb.deallocate(c).unwrap();
        fb.deallocate(b).unwrap();
        fb.deallocate(a).unwrap();
    }
}

#[test]
fn tracking_reports_leaks_by_tag() {
    let sys = SystemAllocator::new();
    let tracked = Tracking::new(&sys, 64).unwrap();
    let _a = tracked.allocate_tagged(10, 8, "cache").unwrap();
    let _b = tracked.allocate_tagged(20, 8, "scene").unwrap();
    let mut out = [qs_memory::TagSummary { tag: "", count: 0, bytes: 0 }; 4];
    let n = tracked.summarize(&mut out);
    assert_eq!(n, 2);
    assert_eq!(tracked.live_count(), 2);
}

#[test]
fn synchronized_allocator_is_shared_across_threads() {
    let shared = Synchronized::new(SystemAllocator::new());
    std::thread::scope(|s| {
        for _ in 0..3 {
            s.spawn(|| {
                for _ in 0..200 {
                    let b = shared.allocate(32, 8).unwrap();
                    // SAFETY: released once by the thread that allocated it.
                    unsafe { shared.deallocate(b).unwrap() };
                }
            });
        }
    });
    assert_eq!(shared.with(|a| a.stats().live_allocations()), 0);
}

#[test]
fn containers_take_storage_from_any_allocator() {
    let sys = SystemAllocator::new();
    {
        let mut v: Vector<String, _> = Vector::new(&sys).unwrap();
        v.push("a".to_string()).unwrap();
        v.push("b".to_string()).unwrap();
        assert_eq!(v.len(), 2);

        let mut m: HashMap<&str, u32, _> = HashMap::new(&sys).unwrap();
        m.insert("x", 1).unwrap();
        assert_eq!(m.get("x"), Some(&1));

        let mut small: SmallVector<u8, 4, _> = SmallVector::new(&sys).unwrap();
        for i in 0..10 {
            small.push(i).unwrap();
        }
        assert!(!small.is_inline());

        let mut ring: RingBuffer<u32, _> = RingBuffer::new(&sys, 2).unwrap();
        ring.push(1).unwrap();
        ring.push(2).unwrap();
        assert_eq!(ring.push(3), Err(3));

        let boxed = OwnedPtr::new(&sys, 99u64).unwrap();
        assert_eq!(*boxed, 99);
    }
    assert_eq!(sys.stats().live_allocations(), 0);
}
