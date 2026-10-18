//! Page-locked allocations (feature 39). Each block's pages are locked in RAM when it is allocated, and
//! unlocked when it is released, so the contents are never written to the page file.
//!
//! Locking is best effort. The OS can refuse (for example at the working-set limit); the block is still
//! returned, and `lock_failures` counts the refusals so callers can see when secrets are not protected.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::AllocResult;
use crate::platform::lock::{lock_pages, unlock_pages};
use core::cell::Cell;

/// Decorator that keeps every live block's pages resident.
pub struct PageLocked<'a, A: Allocator> {
    inner: &'a A,
    lock_failures: Cell<usize>,
}

impl<'a, A: Allocator> PageLocked<'a, A> {
    /// Wraps `inner`.
    pub fn new(inner: &'a A) -> Self {
        PageLocked { inner, lock_failures: Cell::new(0) }
    }

    /// Number of allocations whose pages the OS refused to lock.
    pub fn lock_failures(&self) -> usize {
        self.lock_failures.get()
    }
}

impl<A: Allocator> Allocator for PageLocked<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        let block = self.inner.allocate(size, align)?;
        if !block.is_empty() {
            // SAFETY: the block was just allocated from the wrapped allocator, so its bytes are owned here.
            if !unsafe { lock_pages(block.ptr(), block.size()) } {
                self.lock_failures.set(self.lock_failures.get() + 1);
            }
        }
        Ok(block)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if !block.is_empty() {
            // SAFETY: the caller guarantees the block is live, so its pages are valid to unlock.
            unsafe { unlock_pages(block.ptr(), block.size()) };
        }
        // SAFETY: the caller guarantees the block is live and from the wrapped allocator.
        unsafe { self.inner.deallocate(block) }
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}
