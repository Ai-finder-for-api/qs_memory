//! Randomized model checks for the free list: after every batch of random operations the
//! heap must verify, live blocks must never overlap, and freeing everything must return
//! the heap to a single free block. Seeds are fixed so every failure can be replayed.

use qs_memory::{Allocator, Block, FreeListAllocator};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn overlaps(a: Block, b: Block) -> bool {
    let (a0, a1) = (a.ptr() as usize, a.ptr() as usize + a.size());
    let (b0, b1) = (b.ptr() as usize, b.ptr() as usize + b.size());
    !(a1 <= b0 || b1 <= a0)
}

fn run(seed: u64, steps: usize) {
    let heap = FreeListAllocator::create(128 * 1024).unwrap();
    let mut rng = Rng(seed);
    let mut live: Vec<Block> = Vec::new();
    for step in 0..steps {
        match rng.below(10) {
            0..=4 => {
                let size = 1 + rng.below(900) as usize;
                let align = 1usize << rng.below(7);
                if let Ok(b) = heap.allocate(size, align) {
                    assert!(live.iter().all(|&o| !overlaps(o, b)), "overlap at step {step}");
                    live.push(b);
                }
            }
            5..=7 if !live.is_empty() => {
                let i = rng.below(live.len() as u64) as usize;
                // SAFETY: the block is live and released exactly once.
                unsafe { heap.deallocate(live.swap_remove(i)).unwrap() };
            }
            _ if !live.is_empty() => {
                let i = rng.below(live.len() as u64) as usize;
                let want = 1 + rng.below(1200) as usize;
                // SAFETY: the block is live with the size it was last returned with.
                let _ = unsafe { heap.try_grow(&mut live[i], want) };
            }
            _ => {}
        }
        if step % 50 == 0 {
            heap.verify().unwrap();
        }
    }
    for b in live.drain(..) {
        // SAFETY: each remaining block is live and released once.
        unsafe { heap.deallocate(b).unwrap() };
    }
    heap.verify().unwrap();
    assert_eq!(heap.fragmentation().free_block_count, 1);
    assert_eq!(heap.stats().live_allocations(), 0);
    assert_eq!(heap.stats().accounting_errors, 0);
}

#[test]
fn freelist_random_operations_seed_1() {
    run(0x243F_6A88_85A3_08D3, 4000);
}

#[test]
fn freelist_random_operations_seed_2() {
    run(0x13198A2E_03707344, 4000);
}

#[test]
fn freelist_random_operations_seed_3() {
    run(0xA4093822_299F31D0, 4000);
}
