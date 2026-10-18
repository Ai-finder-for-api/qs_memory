//! Allocator benchmarks: steady-state allocate/free cost per allocator, measured on this machine.
//!
//! Run in release mode: cargo bench --bench allocators

use qs_memory::{Allocator, Arena, BuddyAllocator, FreeListAllocator, PoolAllocator, SlabAllocator, SystemAllocator};
use std::hint::black_box;
use std::time::Instant;

const ITERS: u64 = 2_000_000;
const SIZE: usize = 64;
const ALIGN: usize = 16;

fn steady<A: Allocator>(name: &str, a: &A) {
    let start = Instant::now();
    for _ in 0..ITERS {
        let b = a.allocate(SIZE, ALIGN).expect("allocation");
        black_box(b.ptr());
        // SAFETY: the block came from `a` and is released once.
        unsafe { a.deallocate(b).expect("release") };
    }
    report(name, start.elapsed().as_nanos() as f64 / ITERS as f64);
}

fn report(name: &str, ns: f64) {
    println!("{name:<32} {ns:>10.2} ns/op");
}

fn main() {
    println!("allocator benchmarks: {ITERS} iterations, {SIZE}-byte blocks, alignment {ALIGN}\n");
    steady("SystemAllocator", &SystemAllocator::new());
    steady("PoolAllocator", &PoolAllocator::create(SIZE, ALIGN, 16).unwrap());
    steady("FreeListAllocator", &FreeListAllocator::create(1 << 20).unwrap());
    steady("BuddyAllocator", &BuddyAllocator::create(1 << 20).unwrap());
    steady("SlabAllocator", &SlabAllocator::create(SIZE, ALIGN).unwrap());

    let arena = Arena::new(1 << 20).unwrap();
    let start = Instant::now();
    for i in 0..ITERS {
        let b = arena.allocate(SIZE, ALIGN).expect("allocation");
        black_box(b.ptr());
        if i & 4095 == 4095 {
            arena.reset();
        }
    }
    report("Arena (bump, reset every 4096)", start.elapsed().as_nanos() as f64 / ITERS as f64);
}
