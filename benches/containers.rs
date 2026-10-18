//! Container benchmarks: the crate's containers against the standard library, on this machine.
//!
//! Run in release mode: cargo bench --bench containers

use qs_memory::{HashMap, RingBuffer, SystemAllocator, Vector};
use std::collections::HashMap as StdHashMap;
use std::hint::black_box;
use std::time::Instant;

const PUSHES: u64 = 1_000_000;
const KEYS: u32 = 200_000;

fn report(name: &str, elapsed: std::time::Duration, ops: u64) {
    println!("{name:<36} {:>10.2} ns/op", elapsed.as_nanos() as f64 / ops as f64);
}

fn main() {
    println!("container benchmarks: {PUSHES} pushes, {KEYS} map inserts\n");

    let start = Instant::now();
    let mut std_vec: Vec<u64> = Vec::new();
    for i in 0..PUSHES {
        std_vec.push(i);
    }
    black_box(std_vec.len());
    report("std::vec::Vec<u64> push", start.elapsed(), PUSHES);

    let sys = SystemAllocator::new();
    let start = Instant::now();
    let mut v: Vector<u64, _> = Vector::new(&sys).unwrap();
    for i in 0..PUSHES {
        v.push(i).unwrap();
    }
    black_box(v.len());
    report("qs Vector<u64> push (SystemAllocator)", start.elapsed(), PUSHES);
    drop(v);

    let start = Instant::now();
    let mut std_map: StdHashMap<u32, u32> = StdHashMap::new();
    for i in 0..KEYS {
        std_map.insert(i, i);
    }
    black_box(std_map.len());
    report("std::collections::HashMap insert", start.elapsed(), KEYS as u64);

    let start = Instant::now();
    let mut map: HashMap<u32, u32, _> = HashMap::new(&sys).unwrap();
    for i in 0..KEYS {
        map.insert(i, i).unwrap();
    }
    black_box(map.len());
    report("qs HashMap insert (SystemAllocator)", start.elapsed(), KEYS as u64);
    drop(map);

    let mut ring: RingBuffer<u64, _> = RingBuffer::new(&sys, 1024).unwrap();
    let start = Instant::now();
    for i in 0..PUSHES {
        if ring.push(i).is_err() {
            black_box(ring.pop());
        }
    }
    report("qs RingBuffer push (with pops)", start.elapsed(), PUSHES);
}
