//! Read–write locking (feature 25). Allocations take the write lock. Inspection of the allocator (its
//! statistics, ownership queries, any read-only view) takes a shared read lock, so many readers run in
//! parallel while writers are exclusive.
//!
//! The wrapped allocator must be `Sync` for readers to share it, so this suits allocators such as
//! `SystemAllocator`. Single-threaded allocators with interior `Cell`s should use `Synchronized` instead.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::AllocResult;
use std::sync::{PoisonError, RwLock};

pub struct RwLocked<A: Allocator + Send + Sync> {
    inner: RwLock<A>,
}

impl<A: Allocator + Send + Sync> RwLocked<A> {
    /// Wraps `inner`.
    pub fn new(inner: A) -> Self {
        RwLocked { inner: RwLock::new(inner) }
    }

    /// Runs `f` with shared access to the allocator. Many readers may run at once.
    pub fn read<R>(&self, f: impl FnOnce(&A) -> R) -> R {
        let guard = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        f(&guard)
    }
}

impl<A: Allocator + Send + Sync> Allocator for RwLocked<A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        self.inner.write().unwrap_or_else(PoisonError::into_inner).allocate(size, align)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        // SAFETY: the caller guarantees the block is live and from the wrapped allocator.
        unsafe { self.inner.write().unwrap_or_else(PoisonError::into_inner).deallocate(block) }
    }

    fn capabilities(&self) -> Capabilities {
        self.read(|a| a.capabilities())
    }
}
