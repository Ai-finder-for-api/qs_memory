//! Sampling profiler (feature 16). Records the call site of every N-th allocation, so the cost stays low
//! on hot paths. Totals are scaled by N to estimate the real volume.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::AllocResult;
use core::cell::{Cell, RefCell};
use core::panic::Location;

/// One recorded allocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub file: &'static str,
    pub line: u32,
    pub bytes: usize,
}

/// Decorator that samples every `every`-th allocation.
pub struct Sampled<'a, A: Allocator> {
    inner: &'a A,
    every: usize,
    seen: Cell<usize>,
    samples: RefCell<Vec<Sample>>,
}

impl<'a, A: Allocator> Sampled<'a, A> {
    /// Wraps `inner`, sampling one allocation in `every` (at least one).
    pub fn new(inner: &'a A, every: usize) -> Self {
        Sampled { inner, every: every.max(1), seen: Cell::new(0), samples: RefCell::new(Vec::new()) }
    }

    /// Allocations seen, sampled or not.
    pub fn allocations(&self) -> usize {
        self.seen.get()
    }

    /// The samples taken so far.
    pub fn samples(&self) -> Vec<Sample> {
        self.samples.borrow().clone()
    }

    /// Estimated total bytes requested: the sampled bytes scaled by the sampling rate.
    pub fn estimated_bytes(&self) -> usize {
        self.samples.borrow().iter().map(|s| s.bytes).sum::<usize>() * self.every
    }
}

impl<A: Allocator> Allocator for Sampled<'_, A> {
    #[track_caller]
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        let at = Location::caller();
        let block = self.inner.allocate(size, align)?;
        let count = self.seen.get() + 1;
        self.seen.set(count);
        if count % self.every == 0 {
            self.samples.borrow_mut().push(Sample { file: at.file(), line: at.line(), bytes: block.size() });
        }
        Ok(block)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        // SAFETY: the caller guarantees the block is live and from the wrapped allocator.
        unsafe { self.inner.deallocate(block) }
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}
