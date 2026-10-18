//! Zero-on-free: a block is overwritten with zeros before it goes back to the wrapped allocator, so freed
//! secrets do not linger in memory. The writes are volatile and followed by a compiler fence.
//!
//! Zeroing happens only when the wrapped allocator confirms the block is live (see `LiveBlocks`). A
//! misuse such as releasing an interior pointer, a foreign pointer, or a block already released is
//! refused without touching its memory. The one case this cannot cover is a stale pointer whose memory
//! has since been handed to someone else; that is a contract violation for every allocator.

use crate::allocators::introspect::LiveBlocks;
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::AllocResult;
use core::sync::atomic::{compiler_fence, Ordering};

/// Decorator that zeroes every live block before releasing it.
pub struct Zeroing<'a, A: Allocator + LiveBlocks> {
    inner: &'a A,
}

impl<'a, A: Allocator + LiveBlocks> Zeroing<'a, A> {
    /// Wraps `inner`.
    pub fn new(inner: &'a A) -> Self {
        Zeroing { inner }
    }
}

impl<A: Allocator + LiveBlocks> Allocator for Zeroing<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        self.inner.allocate(size, align)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if self.inner.is_live(block) {
            let ptr = block.ptr();
            for i in 0..block.size() {
                // SAFETY: `is_live` confirmed the block is allocated and at least `block.size()` bytes long.
                unsafe { core::ptr::write_volatile(ptr.add(i), 0) };
            }
            compiler_fence(Ordering::SeqCst);
        }
        // SAFETY: the caller guarantees the block is from the wrapped allocator; a block that is not live
        // is refused by it without any memory being touched here.
        unsafe { self.inner.deallocate(block) }
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}
