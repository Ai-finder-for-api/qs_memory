//! Per-thread caching (feature 2). Each thread is assigned a cache slot. Requests for one size class
//! are served from the thread's slot, and the slot refills from the wrapped allocator in batches, so most
//! allocations do not touch the shared allocator.
//!
//! Trade-offs, stated plainly:
//! - A slot still has a lock. Threads that share a slot contend for it, and the slot count is fixed.
//! - Blocks in a cache are still allocated in the wrapped allocator. They are returned to it by `flush`
//!   or when the cache is dropped.
//! - Releases of the cached size class are not validated before being cached, so a double release of
//!   such a block is not detected until it is handed out again. Use this for performance-critical paths
//!   with correct callers; keep a checked allocator in front of it while debugging.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

const SLOTS: usize = 8;
static NEXT_SLOT: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static SLOT: usize = NEXT_SLOT.fetch_add(1, Ordering::Relaxed) % SLOTS;
}

/// A caching front for one size class of a wrapped allocator.
pub struct ThreadCached<A: Allocator + Send> {
    inner: Mutex<A>,
    class_size: usize,
    class_align: usize,
    batch: usize,
    caches: Vec<Mutex<Vec<Block>>>,
}

impl<A: Allocator + Send> ThreadCached<A> {
    /// Caches blocks of `class_size` bytes aligned to `class_align`, refilling `batch` at a time.
    pub fn new(inner: A, class_size: usize, class_align: usize, batch: usize) -> Self {
        ThreadCached {
            inner: Mutex::new(inner),
            class_size,
            class_align,
            batch: batch.max(1),
            caches: (0..SLOTS).map(|_| Mutex::new(Vec::new())).collect(),
        }
    }

    fn slot(&self) -> &Mutex<Vec<Block>> {
        &self.caches[SLOT.with(|s| *s)]
    }

    /// Returns every cached block to the wrapped allocator.
    pub fn flush(&self) -> AllocResult<()> {
        let mut first: Option<AllocError> = None;
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        for cache in &self.caches {
            let mut blocks = cache.lock().unwrap_or_else(PoisonError::into_inner);
            for block in blocks.drain(..) {
                // SAFETY: cached blocks are live allocations of `inner` that no caller holds.
                if let Err(e) = unsafe { inner.deallocate(block) } {
                    first.get_or_insert(e);
                }
            }
        }
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Number of blocks currently held in caches.
    pub fn cached_blocks(&self) -> usize {
        self.caches.iter().map(|c| c.lock().unwrap_or_else(PoisonError::into_inner).len()).sum()
    }
}

impl<A: Allocator + Send> Allocator for ThreadCached<A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if size != self.class_size || align != self.class_align {
            return self.inner.lock().unwrap_or_else(PoisonError::into_inner).allocate(size, align);
        }
        if let Some(block) = self.slot().lock().unwrap_or_else(PoisonError::into_inner).pop() {
            return Ok(block);
        }
        let mut fresh = Vec::with_capacity(self.batch);
        {
            let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            for _ in 0..self.batch {
                match inner.allocate(self.class_size, self.class_align) {
                    Ok(block) => fresh.push(block),
                    Err(e) => {
                        if fresh.is_empty() {
                            return Err(e);
                        }
                        break;
                    }
                }
            }
        }
        let first = fresh.pop().ok_or(AllocError::CapacityExhausted)?;
        self.slot().lock().unwrap_or_else(PoisonError::into_inner).extend(fresh);
        Ok(first)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if !block.is_empty() && block.size() == self.class_size && block.align() == self.class_align {
            if self.already_cached(block.ptr() as usize) {
                return Err(AllocError::DoubleFree);
            }
            self.slot().lock().unwrap_or_else(PoisonError::into_inner).push(block);
            return Ok(());
        }
        // SAFETY: the caller guarantees the block is live and from the wrapped allocator.
        unsafe { self.inner.lock().unwrap_or_else(PoisonError::into_inner).deallocate(block) }
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner).capabilities()
    }
}

impl<A: Allocator + Send> Drop for ThreadCached<A> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl<A: Allocator + Send> ThreadCached<A> {
    /// True when `addr` is held in any cache slot. Checked on release so a double release is refused.
    fn already_cached(&self, addr: usize) -> bool {
        self.caches
            .iter()
            .any(|c| c.lock().unwrap_or_else(PoisonError::into_inner).iter().any(|b| b.ptr() as usize == addr))
    }
}
