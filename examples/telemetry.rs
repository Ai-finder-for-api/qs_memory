//! Telemetry for a workload: counters, a size histogram and a text report.
//!
//! Run with: cargo run --example telemetry

use qs_memory::{AllocError, Allocator, Block, FreeListAllocator, Telemetry};

fn main() {
    let heap = FreeListAllocator::create(1 << 18).expect("heap");
    let mut telemetry = Telemetry::new();
    let mut live: Vec<Block> = Vec::new();
    let mut seed: u64 = 42;
    for _ in 0..2000 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        if live.is_empty() || (seed >> 33) % 3 != 0 {
            let size = 8 + ((seed >> 40) % 2048) as usize;
            let result = heap.allocate(size, 16);
            telemetry.observe(&result, size);
            if let Ok(b) = result {
                live.push(b);
            }
        } else {
            let i = ((seed >> 20) % live.len() as u64) as usize;
            let b = live.swap_remove(i);
            telemetry.record_free(b.size());
            // SAFETY: the block is live and released once.
            unsafe { heap.deallocate(b).unwrap() };
        }
    }

    let mut report = [0u8; 512];
    let n = telemetry.report(&mut report);
    println!("{}", String::from_utf8_lossy(&report[..n.min(report.len() - 1)]));
    println!("capacity failures: {}", telemetry.failures_of(AllocError::CapacityExhausted));

    for b in live {
        // SAFETY: each remaining block is live and released once.
        unsafe { heap.deallocate(b).unwrap() };
    }
    heap.verify().unwrap();
}
