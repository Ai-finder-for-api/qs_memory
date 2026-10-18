//! General-purpose allocator backed by the C runtime's aligned allocator.
//!
//! Stateless apart from its atomic counters, so it is `Sync` and can be shared
//! freely between threads.

use crate::foundation::align::is_valid_alignment;
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::MAX_BLOCK_SIZE;
use crate::foundation::error::{AllocError, AllocResult};
use crate::foundation::stats::{AtomicStats, Stats};
use crate::platform;

/// Aligned heap allocation through the C runtime.
#[derive(Debug, Default)]
pub struct SystemAllocator {
    stats: AtomicStats,
}

impl SystemAllocator {
    /// Creates a new system allocator with zeroed counters.
    pub const fn new() -> Self {
        Self { stats: AtomicStats::new() }
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.snapshot()
    }

    const CAPABILITIES: Capabilities = Capabilities::ALLOCATE
        .union(Capabilities::INDIVIDUAL_FREE)
        .union(Capabilities::THREAD_SAFE)
        .union(Capabilities::STATISTICS);
}

impl Allocator for SystemAllocator {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !is_valid_alignment(align) {
            self.stats.on_failure();
            return Err(AllocError::InvalidAlignment);
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        if size > MAX_BLOCK_SIZE {
            self.stats.on_failure();
            return Err(AllocError::Overflow);
        }
        let p = platform::raw_allocate(size, align);
        if p.is_null() {
            self.stats.on_failure();
            return Err(AllocError::OutOfMemory);
        }
        self.stats.on_alloc(size);
        Ok(Block::new(p, size, align))
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        if block.ptr().is_null() {
            return Err(AllocError::InvalidArgument);
        }
        if !is_valid_alignment(block.align()) {
            return Err(AllocError::InvalidAlignment);
        }
        if (block.ptr() as usize) & (block.align() - 1) != 0 {
            return Err(AllocError::InvalidFree);
        }
        // SAFETY: the caller guarantees `block` came from this allocator and is live;
        // the checks above rule out the cases we can detect locally.
        unsafe { platform::raw_deallocate(block.ptr()) };
        self.stats.on_free(block.size());
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Self::CAPABILITIES
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_aligned_blocks_and_counts_them() {
        let sys = SystemAllocator::new();
        for align in [1usize, 16, 64, 256] {
            let b = sys.allocate(100, align).unwrap();
            assert_eq!(b.size(), 100);
            assert_eq!(b.ptr() as usize & (align.max(8) - 1), 0);
            // SAFETY: `b` was just returned by `sys` and is released once.
            unsafe { sys.deallocate(b).unwrap() };
        }
        assert_eq!(sys.stats().live_allocations(), 0);
        assert_eq!(sys.stats().allocations, 4);
    }

    #[test]
    fn rejects_bad_requests_without_allocating() {
        let sys = SystemAllocator::new();
        assert_eq!(sys.allocate(16, 3).unwrap_err(), AllocError::InvalidAlignment);
        assert_eq!(sys.allocate(MAX_BLOCK_SIZE + 1, 8).unwrap_err(), AllocError::Overflow);
        assert!(sys.allocate(0, 8).unwrap().is_empty());
        assert_eq!(sys.stats().allocations, 0);
    }

    #[test]
    fn misuse_is_reported() {
        let sys = SystemAllocator::new();
        let b = sys.allocate(64, 16).unwrap();
        // SAFETY: the block is passed to `deallocate` once, below, after the bad call.
        unsafe {
            let shifted = Block::from_raw_parts(b.ptr().add(4), 64, 16);
            assert_eq!(sys.deallocate(shifted), Err(AllocError::InvalidFree));
            assert_eq!(
                sys.deallocate(Block::from_raw_parts(core::ptr::null_mut(), 8, 8)),
                Err(AllocError::InvalidArgument)
            );
            sys.deallocate(b).unwrap();
        }
        assert_eq!(sys.stats().live_allocations(), 0);
    }
}
