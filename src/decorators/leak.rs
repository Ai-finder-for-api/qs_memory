//! Leak check on drop. A `LeakCheck` counts the blocks it hands out and the ones it gets back. If it is
//! dropped while blocks are still live, it reports them on standard error. Optionally it panics
//! instead, so tests fail loudly.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::AllocResult;
use core::cell::Cell;

/// Decorator that reports blocks still live when it is dropped.
pub struct LeakCheck<'a, A: Allocator> {
    name: &'static str,
    inner: &'a A,
    live_blocks: Cell<usize>,
    live_bytes: Cell<usize>,
    panic_on_leak: bool,
}

impl<'a, A: Allocator> LeakCheck<'a, A> {
    /// Wraps `inner`. `name` labels the report.
    pub fn new(name: &'static str, inner: &'a A) -> Self {
        LeakCheck { name, inner, live_blocks: Cell::new(0), live_bytes: Cell::new(0), panic_on_leak: false }
    }

    /// Makes dropping with live blocks panic instead of printing. A panic during unwinding is never
    /// raised a second time.
    pub fn panic_on_leak(mut self) -> Self {
        self.panic_on_leak = true;
        self
    }

    /// Blocks handed out and not yet returned.
    pub fn live_blocks(&self) -> usize {
        self.live_blocks.get()
    }

    /// Bytes handed out and not yet returned.
    pub fn live_bytes(&self) -> usize {
        self.live_bytes.get()
    }

    /// True when every block has been returned.
    pub fn is_clean(&self) -> bool {
        self.live_blocks.get() == 0
    }

    /// Describes the outstanding blocks, or `None` when there are none.
    pub fn leak_message(&self) -> Option<String> {
        if self.is_clean() {
            None
        } else {
            Some(format!(
                "leak check '{}': {} block(s), {} byte(s) still live",
                self.name,
                self.live_blocks.get(),
                self.live_bytes.get()
            ))
        }
    }
}

impl<A: Allocator> Allocator for LeakCheck<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        let block = self.inner.allocate(size, align)?;
        if !block.is_empty() {
            self.live_blocks.set(self.live_blocks.get() + 1);
            self.live_bytes.set(self.live_bytes.get() + block.size());
        }
        Ok(block)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        // SAFETY: the caller guarantees the block is live and from the wrapped allocator.
        unsafe { self.inner.deallocate(block)? };
        if !block.is_empty() {
            self.live_blocks.set(self.live_blocks.get().saturating_sub(1));
            self.live_bytes.set(self.live_bytes.get().saturating_sub(block.size()));
        }
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}

impl<A: Allocator> Drop for LeakCheck<'_, A> {
    fn drop(&mut self) {
        if let Some(message) = self.leak_message() {
            if self.panic_on_leak && !std::thread::panicking() {
                panic!("{message}");
            }
            eprintln!("{message}");
        }
    }
}
