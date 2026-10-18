//! Ring (streaming) allocator: a circular buffer for producer–consumer data.
//!
//! Blocks are handed out in order and must be released in the same order, oldest first. Releasing
//! out of order is refused with `OutOfOrderFree`. When a block does not fit at the end of the buffer,
//! the unused tail is skipped and the block starts at the beginning. The skipped bytes are reclaimed
//! together with the block that caused the skip.
//!
//! Every block is a multiple of 16 bytes and starts on a 16-byte boundary, so the alignment limit is 16.

use crate::allocators::system::SystemAllocator;
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use core::cell::{Cell, RefCell};
use std::collections::VecDeque;

struct Entry {
    addr: usize,
    span: u64,
}

/// A fixed-size circular buffer with in-order allocation and release.
pub struct RingAllocator {
    backing: SystemAllocator,
    region: Block,
    base: usize,
    cap: u64,
    head: Cell<u64>,
    tail: Cell<u64>,
    entries: RefCell<VecDeque<Entry>>,
}

impl RingAllocator {
    /// Creates a ring of `capacity` bytes (rounded down to 16).
    pub fn new(capacity: usize) -> AllocResult<Self> {
        let cap = capacity & !15;
        if cap == 0 {
            return Err(AllocError::InvalidArgument);
        }
        let backing = SystemAllocator::new();
        let region = backing.allocate(cap, 16)?;
        Ok(RingAllocator {
            backing,
            region,
            base: region.ptr() as usize,
            cap: cap as u64,
            head: Cell::new(0),
            tail: Cell::new(0),
            entries: RefCell::new(VecDeque::new()),
        })
    }

    /// Size of the ring in bytes.
    pub fn capacity(&self) -> usize {
        self.cap as usize
    }

    /// Bytes between the oldest live block and the newest, including skipped tails.
    pub fn used_bytes(&self) -> usize {
        (self.head.get() - self.tail.get()) as usize
    }

    /// Number of blocks not yet released.
    pub fn live_blocks(&self) -> usize {
        self.entries.borrow().len()
    }
}

impl Allocator for RingAllocator {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !align.is_power_of_two() {
            return Err(AllocError::InvalidAlignment);
        }
        if align > 16 {
            return Err(AllocError::NotSupported);
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        let need = (size.checked_add(15).ok_or(AllocError::Overflow)? & !15) as u64;
        if need > self.cap {
            return Err(AllocError::CapacityExhausted);
        }
        let head = self.head.get();
        let pos = head % self.cap;
        let pad = if pos + need > self.cap { self.cap - pos } else { 0 };
        if (head - self.tail.get()) + pad + need > self.cap {
            return Err(AllocError::CapacityExhausted);
        }
        let start = head + pad;
        self.head.set(start + need);
        let addr = self.base + (start % self.cap) as usize;
        self.entries.borrow_mut().push_back(Entry { addr, span: pad + need });
        // SAFETY: `addr .. addr + need` lies inside the region, because the block does not wrap.
        Ok(unsafe { Block::from_raw_parts(addr as *mut u8, need as usize, align) })
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return Ok(());
        }
        let addr = block.ptr() as usize;
        let mut entries = self.entries.borrow_mut();
        let is_oldest = entries.front().map_or(false, |e| e.addr == addr);
        if !is_oldest {
            let known = entries.iter().any(|e| e.addr == addr);
            return Err(if known { AllocError::OutOfOrderFree } else { AllocError::InvalidFree });
        }
        let Some(oldest) = entries.pop_front() else {
            return Err(AllocError::InvalidFree);
        };
        self.tail.set(self.tail.get() + oldest.span);
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE.union(Capabilities::INDIVIDUAL_FREE)
    }
}

impl Drop for RingAllocator {
    fn drop(&mut self) {
        // SAFETY: `region` came from `backing` and is released exactly once, here.
        let _ = unsafe { self.backing.deallocate(self.region) };
    }
}

impl crate::allocators::introspect::LiveBlocks for RingAllocator {
    fn is_live(&self, block: Block) -> bool {
        if block.is_empty() {
            return false;
        }
        let addr = block.ptr() as usize;
        self.entries.borrow().iter().any(|e| e.addr == addr && block.size() as u64 <= e.span)
    }
}

impl crate::decorators::Ownership for RingAllocator {
    fn owns(&self, p: *const u8, n: usize) -> bool {
        let start = self.base;
        let addr = p as usize;
        addr >= start && addr.checked_add(n).map_or(false, |end| end <= start + self.cap as usize)
    }
}