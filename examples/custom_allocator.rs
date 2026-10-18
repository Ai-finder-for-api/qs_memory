//! A custom allocator: any type that implements `Allocator` can back containers.
//! This one is a bump allocator over a static buffer that never frees individual blocks.
//!
//! Run with: cargo run --example custom_allocator

use qs_memory::{AllocError, AllocResult, Allocator, Block, Capabilities, Vector};
use std::cell::Cell;

struct StaticBump {
    buf: std::cell::UnsafeCell<[u8; 4096]>,
    top: Cell<usize>,
}

impl StaticBump {
    const fn new() -> Self {
        StaticBump { buf: std::cell::UnsafeCell::new([0; 4096]), top: Cell::new(0) }
    }
}

impl Allocator for StaticBump {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !align.is_power_of_two() {
            return Err(AllocError::InvalidAlignment);
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        let base = self.buf.get() as usize;
        let start = (base + self.top.get()).next_multiple_of(align) - base;
        if start > 4096 || size > 4096 - start {
            return Err(AllocError::CapacityExhausted);
        }
        self.top.set(start + size);
        // SAFETY: `start + size` is inside the buffer, which lives for the program's lifetime.
        Ok(unsafe { Block::from_raw_parts((self.buf.get() as *mut u8).add(start), size, align) })
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            Ok(())
        } else {
            Err(AllocError::NotSupported)
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE
    }
}

fn main() {
    let bump = StaticBump::new();
    let mut squares: Vector<i32, StaticBump> = Vector::new(&bump).expect("vector");
    for i in 0..100 {
        squares.push(i * i).expect("room in the static buffer");
    }
    let sum: i64 = squares.iter().map(|&x| x as i64).sum();
    println!("stored {} squares, sum = {sum}", squares.len());
    assert_eq!(sum, 328_350);
}
