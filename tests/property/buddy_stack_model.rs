//! Randomized model checks for the buddy heap and the stack allocator.
//!
//! Buddy: the side table must tile the region and merge buddies after every batch.
//! Stack: checkpoints must restore exactly the live set recorded when they were taken;
//! rolling back past a checkpoint's block makes it stale, and it is then dropped from the
//! model. Seeds are fixed for replay.

use qs_memory::{Allocator, Block, BuddyAllocator, StackAllocator, StackCheckpoint};

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

#[test]
fn buddy_random_operations_keep_the_side_table_consistent() {
    let heap = BuddyAllocator::create(1 << 17).unwrap();
    let mut rng = Rng(0x0BAD_C0DE_1234_5678);
    let mut live: Vec<Block> = Vec::new();
    for step in 0..3000 {
        if live.is_empty() || rng.below(5) < 3 {
            let size = 1 + rng.below(2000) as usize;
            let align = 1usize << rng.below(6);
            if let Ok(b) = heap.allocate(size, align) {
                assert_eq!(b.ptr() as usize & (align - 1), 0);
                live.push(b);
            }
        } else {
            let i = rng.below(live.len() as u64) as usize;
            // SAFETY: the block is live and released once.
            unsafe { heap.deallocate(live.swap_remove(i)).unwrap() };
        }
        if step % 40 == 0 {
            heap.verify().unwrap();
        }
    }
    for b in live.drain(..) {
        // SAFETY: each remaining block is live and released once.
        unsafe { heap.deallocate(b).unwrap() };
    }
    heap.verify().unwrap();
    assert_eq!(heap.structure().free_blocks, 1);
}

#[test]
fn stack_checkpoints_restore_the_recorded_live_set() {
    let stack = StackAllocator::new(1 << 16).unwrap();
    let mut rng = Rng(0x5EED_0F57_AC4_u64);
    // Model: live blocks in allocation order, and checkpoints with the live count they saw.
    let mut model: Vec<Block> = Vec::new();
    let mut marks: Vec<(StackCheckpoint, usize)> = Vec::new();
    for _ in 0..2500 {
        match rng.below(10) {
            0..=4 => {
                let size = 1 + rng.below(200) as usize;
                if let Ok(b) = stack.allocate(size, 8) {
                    model.push(b);
                }
            }
            5..=6 if !model.is_empty() => {
                let b = model.pop().unwrap();
                // SAFETY: the newest live block is released; LIFO order is respected.
                unsafe { stack.deallocate(b).unwrap() };
                // Checkpoints that recorded more blocks than remain are now stale.
                marks.retain(|(_, count)| *count <= model.len());
            }
            7..=8 => marks.push((stack.checkpoint(), model.len())),
            _ if !marks.is_empty() => {
                let i = rng.below(marks.len() as u64) as usize;
                let (cp, count) = marks[i];
                marks.truncate(i);
                match stack.rollback(cp) {
                    Ok(()) => model.truncate(count),
                    Err(e) => panic!("rollback refused: {e}"),
                }
            }
            _ => {}
        }
        assert_eq!(stack.live_blocks() as usize, model.len());
    }
}
