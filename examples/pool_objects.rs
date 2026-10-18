//! Typed objects in a fixed-size pool, owned through `OwnedPtr` so they are released automatically.
//!
//! Run with: cargo run --example pool_objects

use qs_memory::{OwnedPtr, PoolAllocator};

#[derive(Debug)]
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
}

fn main() {
    let pool = PoolAllocator::create(std::mem::size_of::<Particle>(), std::mem::align_of::<Particle>(), 128)
        .expect("pool");
    let mut live: Vec<OwnedPtr<Particle, PoolAllocator>> = Vec::new();
    for i in 0..100 {
        let p = OwnedPtr::new(&pool, Particle { x: i as f32, y: 0.0, vx: 1.0, vy: 0.5 }).expect("slot");
        live.push(p);
    }
    println!("created 100 particles; pool holds {} of {} slots", pool.slots_in_use(), pool.slot_count());

    // Dropping every other particle returns its slot to the pool immediately.
    let mut kept = Vec::new();
    for (i, p) in live.into_iter().enumerate() {
        if i % 2 == 0 {
            drop(p);
        } else {
            kept.push(p);
        }
    }
    let checksum: f32 = kept.iter().map(|p| p.x + p.y + p.vx + p.vy).sum();
    println!("after dropping even indices: {} slots in use (checksum {checksum})", pool.slots_in_use());
    drop(kept);
    assert_eq!(pool.slots_in_use(), 0);
}
