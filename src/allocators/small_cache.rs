//! Small-object cache (feature 45). A single-threaded front for one size class: releases of that class are
//! kept in a bounded list and handed back out before the wrapped allocator is asked again. Each release is
//! checked against the cached blocks, so releasing the same block twice is refused while it is cached.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use core::cell::{Cell, RefCell};

pub struct SmallCache<'a, A: Allocator> {
    inner: &'a A,
    class_size: usize,
    class_align: usize,
    limit: usize,
    held: RefCell<Vec<Block>>,
    hits: Cell<usize>,
    misses: Cell<usize>,
}

impl<'a, A: Allocator> SmallCache<'a, A> {
    /// Caches up to `limit` released blocks of `class_size` bytes aligned to `class_align`.
    pub fn new(inner: &'a A, class_size: usize, class_align: usize, limit: usize) -> Self {
        SmallCache {
            inner,
            class_size,
            class_align,
            limit,
            held: RefCell::new(Vec::new()),
            hits: Cell::new(0),
            misses: Cell::new(0),
        }
    }

    /// Requests served from the cache.
    pub fn hits(&self) -> usize {
        self.hits.get()
    }

    /// Requests that went to the wrapped allocator.
    pub fn misses(&self) -> usize {
        self.misses.get()
    }

    /// Blocks currently held.
    pub fn cached(&self) -> usize {
        self.held.borrow().len()
    }

    fn is_class(&self, size: usize, align: usize) -> bool {
        size == self.class_size && align == self.class_align
    }

    /// Returns every cached block to the wrapped allocator.
    pub fn flush(&self) -> AllocResult<()> {
        let mut first: Option<AllocError> = None;
        for block in self.held.borrow_mut().drain(..) {
            // SAFETY: cached blocks are live allocations of the wrapped allocator that no caller holds.
            if let Err(e) = unsafe { self.inner.deallocate(block) } {
                first.get_or_insert(e);
            }
        }
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl<A: Allocator> Allocator for SmallCache<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if self.is_class(size, align) {
            if let Some(block) = self.held.borrow_mut().pop() {
                self.hits.set(self.hits.get() + 1);
                return Ok(block);
            }
            self.misses.set(self.misses.get() + 1);
        }
        self.inner.allocate(size, align)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if !block.is_empty() && self.is_class(block.size(), block.align()) {
            let mut held = self.held.borrow_mut();
            if held.iter().any(|b| b.ptr() == block.ptr()) {
                return Err(AllocError::DoubleFree);
            }
            if held.len() < self.limit {
                held.push(block);
                return Ok(());
            }
        }
        // SAFETY: the caller guarantees the block is live and from the wrapped allocator.
        unsafe { self.inner.deallocate(block) }
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}

impl<A: Allocator> Drop for SmallCache<'_, A> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}
