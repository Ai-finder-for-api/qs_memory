//! Structured fuzz driver. Each seed expands into a byte program that drives an allocator with valid and
//! invalid operations. A shadow model checks every outcome, and every heap is verified after each batch.
//! Seeds are fixed so every failure can be replayed.
//!
//! This is not coverage-guided fuzzing. For longer sessions, raise the seed count with QS_FUZZ_SEEDS.

use qs_memory::{AllocError, Allocator, Block, FreeListAllocator, Tlsf};

struct Program {
    data: Vec<u8>,
    pos: usize,
}

impl Program {
    fn from_seed(seed: u64, len: usize) -> Self {
        let mut s = seed | 1;
        let data = (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s >> 24) as u8
            })
            .collect();
        Program { data, pos: 0 }
    }

    fn byte(&mut self) -> u8 {
        let b = self.data[self.pos % self.data.len()];
        self.pos += 1;
        b
    }

    fn word(&mut self) -> usize {
        ((self.byte() as usize) << 8) | self.byte() as usize
    }
}

fn seeds() -> u64 {
    std::env::var("QS_FUZZ_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(24)
}

fn fuzz_tlsf(program: &mut Program, steps: usize) {
    let heap = Tlsf::new(1 << 16).unwrap();
    let mut live: Vec<(Block, usize)> = Vec::new();
    let mut freed: Vec<Block> = Vec::new();
    let outside = [0u8; 64];
    for step in 0..steps {
        match program.byte() % 7 {
            0 | 1 => {
                let size = 1 + program.word() % 2000;
                let align = 1usize << (program.byte() % 8);
                if let Ok(block) = heap.allocate(size, align) {
                    assert_eq!(block.ptr() as usize % align, 0, "step {step}: alignment");
                    assert!(block.size() >= size, "step {step}: size");
                    live.push((block, size));
                }
            }
            2 => {
                if !live.is_empty() {
                    let i = program.word() % live.len();
                    let (block, _) = live.swap_remove(i);
                    // SAFETY: the block is live and released once here.
                    unsafe { heap.deallocate(block).unwrap() };
                    freed.push(block);
                }
            }
            3 => {
                // Replay a released block. Skip it when its address has been handed out again, because then
                // the release would legitimately free someone else's block; the heap's guarantees cover the rest.
                if !freed.is_empty() {
                    let block = freed[program.word() % freed.len()];
                    if !live.iter().any(|(l, _)| l.ptr() == block.ptr()) {
                        // SAFETY: deliberate misuse; the heap must refuse it without corrupting itself.
                        let result = unsafe { heap.deallocate(block) };
                        assert!(
                            matches!(result, Err(AllocError::DoubleFree) | Err(AllocError::InvalidFree)),
                            "step {step}: replayed release was accepted: {result:?}"
                        );
                    }
                }
            }
            4 => {
                // A pointer outside the heap: refused without being read.
                let p = outside.as_ptr().wrapping_add(program.byte() as usize % 64) as *mut u8;
                // SAFETY: deliberate misuse with a stack address; refused before any access.
                let result = unsafe { heap.deallocate(Block::from_raw_parts(p, 8, 8)) };
                assert_eq!(result, Err(AllocError::NotOwned), "step {step}");
            }
            5 => {
                // An interior pointer into a live block: refused without touching the block.
                if let Some((block, _)) = live.first().copied() {
                    if block.size() > 32 {
                        let interior = block.ptr().wrapping_add(16);
                        // SAFETY: deliberate misuse; the heap refuses it because it is not a block start.
                        let result = unsafe { heap.deallocate(Block::from_raw_parts(interior, 16, 16)) };
                        assert_eq!(result, Err(AllocError::InvalidFree), "step {step}");
                    }
                }
            }
            _ => heap.verify().unwrap(),
        }
        if step % 64 == 0 {
            heap.verify().unwrap();
        }
    }
    for (block, _) in live.drain(..) {
        // SAFETY: each remaining block is live and released once.
        unsafe { heap.deallocate(block).unwrap() };
    }
    heap.verify().unwrap();
    assert_eq!(heap.free_block_count(), 1);
}

fn fuzz_freelist(program: &mut Program, steps: usize) {
    let heap = FreeListAllocator::create(1 << 16).unwrap();
    let mut live: Vec<Block> = Vec::new();
    for step in 0..steps {
        match program.byte() % 4 {
            0 | 1 => {
                let size = 1 + program.word() % 1500;
                if let Ok(block) = heap.allocate(size, 16) {
                    live.push(block);
                }
            }
            2 => {
                if !live.is_empty() {
                    let i = program.word() % live.len();
                    let block = live.swap_remove(i);
                    // SAFETY: the block is live and released once.
                    unsafe { heap.deallocate(block).unwrap() };
                }
            }
            _ => heap.verify().unwrap(),
        }
        if step % 64 == 0 {
            heap.verify().unwrap();
        }
    }
    for block in live.drain(..) {
        // SAFETY: each remaining block is live and released once.
        unsafe { heap.deallocate(block).unwrap() };
    }
    heap.verify().unwrap();
    assert_eq!(heap.fragmentation().free_block_count, 1);
}

#[test]
fn tlsf_survives_many_fixed_seed_programs() {
    for seed in 0..seeds() {
        let mut program = Program::from_seed(0x9E37_79B9_7F4A_7C15 ^ seed, 8192);
        fuzz_tlsf(&mut program, 3000);
    }
}

#[test]
fn freelist_survives_many_fixed_seed_programs() {
    for seed in 0..seeds() {
        let mut program = Program::from_seed(0xC2B2_AE3D_27D4_EB4F ^ seed, 8192);
        fuzz_freelist(&mut program, 3000);
    }
}
