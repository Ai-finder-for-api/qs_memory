//! Sharded allocator (feature 3). Several independent heaps, each behind its own lock. Each thread starts
//! at its own shard, so threads mostly take different locks. A full shard spills into the next one.
//!
//! Releases find the owning shard with `Ownership::owns`, so a block can be released from any thread.

use crate::allocators::traits::Allocator;
use crate::decorators::Ownership;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

static NEXT_START: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static START: usize = NEXT_START.fetch_add(1, Ordering::Relaxed);
}

/// A set of shards, each an allocator that owns its own memory.
pub struct Sharded<A: Allocator + Ownership + Send> {
    shards: Vec<Mutex<A>>,
}

impl<A: Allocator + Ownership + Send> Sharded<A> {
    /// Builds the set from `shards`. Shards must own disjoint memory.
    pub fn new(shards: Vec<A>) -> Self {
        Sharded { shards: shards.into_iter().map(Mutex::new).collect() }
    }

    /// Number of shards.
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }
}

impl<A: Allocator + Ownership + Send> Allocator for Sharded<A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        let n = self.shards.len();
        if n == 0 {
            return Err(AllocError::CapacityExhausted);
        }
        let start = START.with(|s| *s) % n;
        let mut last = AllocError::CapacityExhausted;
        for k in 0..n {
            let shard = &self.shards[(start + k) % n];
            match shard.lock().unwrap_or_else(PoisonError::into_inner).allocate(size, align) {
                Ok(block) => return Ok(block),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return Ok(());
        }
        for shard in &self.shards {
            let guard = shard.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.owns(block.ptr(), block.size()) {
                // SAFETY: the caller guarantees the block is live; the owning shard is the one that
                // allocated it, so it is released to the right allocator.
                return unsafe { guard.deallocate(block) };
            }
        }
        Err(AllocError::NotOwned)
    }

    fn capabilities(&self) -> Capabilities {
        self.shards
            .first()
            .map_or(Capabilities::NONE, |s| s.lock().unwrap_or_else(PoisonError::into_inner).capabilities())
    }
}
