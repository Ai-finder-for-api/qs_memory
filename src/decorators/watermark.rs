//! Peak tracking: the highest live byte and block counts seen since the last reset.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::AllocResult;
use core::cell::Cell;

/// A snapshot of current and peak usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Peaks {
    pub live_bytes: usize,
    pub live_blocks: usize,
    pub peak_bytes: usize,
    pub peak_blocks: usize,
}

/// Decorator that records the highest usage it has seen.
pub struct Watermark<'a, A: Allocator> {
    inner: &'a A,
    live_bytes: Cell<usize>,
    live_blocks: Cell<usize>,
    peak_bytes: Cell<usize>,
    peak_blocks: Cell<usize>,
}

impl<'a, A: Allocator> Watermark<'a, A> {
    /// Wraps `inner` with all counters at zero.
    pub fn new(inner: &'a A) -> Self {
        Watermark {
            inner,
            live_bytes: Cell::new(0),
            live_blocks: Cell::new(0),
            peak_bytes: Cell::new(0),
            peak_blocks: Cell::new(0),
        }
    }

    /// Current and peak usage.
    pub fn peaks(&self) -> Peaks {
        Peaks {
            live_bytes: self.live_bytes.get(),
            live_blocks: self.live_blocks.get(),
            peak_bytes: self.peak_bytes.get(),
            peak_blocks: self.peak_blocks.get(),
        }
    }

    /// Sets each peak to the current value, so the next peak is measured from now.
    pub fn reset_peak(&self) {
        self.peak_bytes.set(self.live_bytes.get());
        self.peak_blocks.set(self.live_blocks.get());
    }

    fn record_alloc(&self, bytes: usize) {
        let bytes_now = self.live_bytes.get() + bytes;
        let blocks_now = self.live_blocks.get() + 1;
        self.live_bytes.set(bytes_now);
        self.live_blocks.set(blocks_now);
        self.peak_bytes.set(self.peak_bytes.get().max(bytes_now));
        self.peak_blocks.set(self.peak_blocks.get().max(blocks_now));
    }
}

impl<A: Allocator> Allocator for Watermark<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        let block = self.inner.allocate(size, align)?;
        if !block.is_empty() {
            self.record_alloc(block.size());
        }
        Ok(block)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        // SAFETY: the caller guarantees the block is live and from the wrapped allocator.
        unsafe { self.inner.deallocate(block)? };
        if !block.is_empty() {
            self.live_bytes.set(self.live_bytes.get().saturating_sub(block.size()));
            self.live_blocks.set(self.live_blocks.get().saturating_sub(1));
        }
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}
