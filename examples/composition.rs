//! Composing allocators: a pool serves small blocks, the system heap serves the rest under a
//! byte budget, and every block is guarded against overruns.
//!
//! Run with: cargo run --example composition

use qs_memory::{Allocator, Budget, Fallback, Guarded, PoolAllocator, SystemAllocator};

fn main() {
    let sys = SystemAllocator::new();
    let budget = Budget::new(&sys, 1 << 20);
    // A guarded request of 48 bytes needs 128 bytes of slot space (header, canaries, padding).
    let pool = PoolAllocator::create(128, 16, 32).expect("pool");
    let tiered = Fallback::new(&pool, &budget);
    let guarded = Guarded::new(&tiered);

    let mut small = Vec::new();
    for i in 0..8u8 {
        let b = guarded.allocate(48, 16).expect("small block");
        // SAFETY: the block is live and at least 48 bytes long.
        unsafe { b.as_mut_slice().fill(i) };
        small.push(b);
    }
    let large = guarded.allocate(8192, 64).expect("large block");
    // SAFETY: the block is live and 8192 bytes long.
    unsafe { large.as_mut_slice().fill(0x7F) };

    println!("pool slots in use: {}, budget live bytes: {}", pool.slots_in_use(), budget.live_bytes());
    println!("guard check on the large block: {:?}", guarded.check(large));

    for b in small {
        // SAFETY: each small block is live and released once.
        unsafe { guarded.deallocate(b).unwrap() };
    }
    // SAFETY: the large block is live and released once.
    unsafe { guarded.deallocate(large).unwrap() };
    println!("after cleanup: pool slots {}, budget live bytes {}", pool.slots_in_use(), budget.live_bytes());
    assert_eq!(pool.slots_in_use(), 0);
    assert_eq!(budget.live_bytes(), 0);
}
