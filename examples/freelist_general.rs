//! General-purpose allocation with the coalescing free list: churn, fragmentation, and verification.
//!
//! Run with: cargo run --example freelist_general

use qs_memory::{Allocator, Block, FreeListAllocator};

fn main() {
    let heap = FreeListAllocator::create(1 << 20).expect("heap");
    let mut kept: Vec<Block> = Vec::new();
    let mut dropped: Vec<Block> = Vec::new();
    for i in 0..64usize {
        let b = heap.allocate(100 + i * 37, 16).expect("allocation");
        if i % 3 == 0 {
            kept.push(b);
        } else {
            dropped.push(b);
        }
    }
    for b in dropped.drain(..) {
        // SAFETY: each block is live and released once.
        unsafe { heap.deallocate(b).unwrap() };
    }

    let f = heap.fragmentation();
    println!(
        "after churn: {} free blocks, {} live blocks, external fragmentation {:.2}",
        f.free_block_count,
        f.allocated_block_count,
        f.external_fragmentation()
    );
    println!("heap verify: {}", if heap.verify().is_ok() { "ok" } else { "CORRUPT" });

    for b in kept.drain(..) {
        // SAFETY: each block is live and released once.
        unsafe { heap.deallocate(b).unwrap() };
    }
    println!("after releasing everything: {} free block(s)", heap.fragmentation().free_block_count);
    assert_eq!(heap.fragmentation().free_block_count, 1);
}
