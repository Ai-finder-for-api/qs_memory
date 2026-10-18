//! Segregated-fit allocator: one fixed-size pool per size class, plus a fallback.
//!
//! Classes: 16, 32, 64, 128, 256, 512, 1024, 2048 and 4096 bytes. A request goes to the
//! smallest class that holds it when its alignment is at most 16. Requests that do not
//! fit, or whose class pool is full, use the fallback: an aligned allocation with a
//! 32-byte prefix recording size, alignment and a magic value.
//!
//! Safety note: deallocating a pointer that this allocator never returned is undefined
//! behaviour, because the fallback reads the prefix before the pointer. Pool-owned
//! pointers are checked exactly by range and occupancy before anything is read.
//! Not `Sync`.

use core::cell::Cell;
use core::mem::size_of;
use core::ptr;

use crate::foundation::align::{align_up, is_valid_alignment};
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::{DEFAULT_ALIGNMENT, MAX_BLOCK_SIZE};
use crate::foundation::error::{AllocError, AllocResult};
use crate::allocators::pool::PoolAllocator;
use crate::foundation::stats::Stats;
use crate::platform;

const CLASS_COUNT: usize = 9;
const CLASS_SIZES: [usize; CLASS_COUNT] = [16, 32, 64, 128, 256, 512, 1024, 2048, 4096];
const MAX_CLASS_ALIGN: usize = 16;
const FALLBACK_PREFIX: usize = 32;
const FALLBACK_MAGIC: u64 = 0x5153_4346_414C_4C42; // "QSCFALLB"

#[repr(C)]
#[derive(Clone, Copy)]
struct FallbackHeader {
    magic: u64,
    size: usize,
    alignment: usize,
    prefix: usize,
}

const _: () = assert!(size_of::<FallbackHeader>() == FALLBACK_PREFIX, "fallback prefix layout");

/// Pools for common sizes plus an aligned fallback for everything else.
pub struct SizeClassAllocator {
    pools: [Option<PoolAllocator>; CLASS_COUNT],
    stats: Cell<Stats>,
    pooled: Cell<u64>,
    fallback: Cell<u64>,
}

impl SizeClassAllocator {
    /// Creates the pools, each with `slots_per_class` slots.
    pub fn create(slots_per_class: usize) -> AllocResult<Self> {
        let mut pools: [Option<PoolAllocator>; CLASS_COUNT] = core::array::from_fn(|_| None);
        for (slot, &class) in pools.iter_mut().zip(CLASS_SIZES.iter()) {
            *slot = Some(PoolAllocator::create(class, MAX_CLASS_ALIGN, slots_per_class)?);
        }
        Ok(SizeClassAllocator {
            pools,
            stats: Cell::new(Stats::default()),
            pooled: Cell::new(0),
            fallback: Cell::new(0),
        })
    }

    /// Index of the smallest class that holds `size`, or `CLASS_COUNT` when none does.
    pub fn class_index_for(size: usize) -> usize {
        CLASS_SIZES.iter().position(|&c| size <= c).unwrap_or(CLASS_COUNT)
    }

    /// Number of size classes.
    pub const fn class_count() -> usize {
        CLASS_COUNT
    }

    /// The pool serving class `index`, if any.
    pub fn pool(&self, index: usize) -> Option<&PoolAllocator> {
        self.pools.get(index).and_then(|p| p.as_ref())
    }

    /// Requests served by a class pool.
    pub fn pooled_allocations(&self) -> u64 {
        self.pooled.get()
    }

    /// Requests served by the fallback.
    pub fn fallback_allocations(&self) -> u64 {
        self.fallback.get()
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    /// True when any class pool owns `[p, p + n)`.
    pub fn owns_pooled(&self, p: *const u8, n: usize) -> bool {
        self.pools.iter().flatten().any(|pool| pool.owns(p, n))
    }

    fn with_stats(&self, f: impl FnOnce(&mut Stats)) {
        let mut s = self.stats.get();
        f(&mut s);
        self.stats.set(s);
    }

    fn fail(&self, e: AllocError) -> AllocError {
        self.with_stats(|s| s.on_failure());
        e
    }
}

impl Allocator for SizeClassAllocator {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !is_valid_alignment(align) {
            return Err(self.fail(AllocError::InvalidAlignment));
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        if size > MAX_BLOCK_SIZE {
            return Err(self.fail(AllocError::Overflow));
        }

        let idx = Self::class_index_for(size);
        if idx < CLASS_COUNT && align <= MAX_CLASS_ALIGN {
            if let Some(pool) = self.pool(idx) {
                if let Ok(b) = pool.allocate(size, align) {
                    self.pooled.set(self.pooled.get() + 1);
                    self.with_stats(|s| s.on_alloc(size));
                    return Ok(Block::new(b.ptr(), size, align));
                }
            }
        }

        // Fallback: aligned allocation with a prefix describing the block.
        let eff = align.max(MAX_CLASS_ALIGN);
        let prefix = align_up(FALLBACK_PREFIX, eff).ok_or(AllocError::Overflow)?;
        let total = prefix.checked_add(size).ok_or(AllocError::Overflow)?;
        let base = platform::raw_allocate(total, eff);
        if base.is_null() {
            return Err(self.fail(AllocError::OutOfMemory));
        }
        let header = FallbackHeader { magic: FALLBACK_MAGIC, size, alignment: eff, prefix };
        // SAFETY: `base + prefix - 32 .. base + prefix` lies inside the allocation.
        unsafe { ptr::write_unaligned(base.add(prefix - FALLBACK_PREFIX) as *mut FallbackHeader, header) };
        self.fallback.set(self.fallback.get() + 1);
        self.with_stats(|s| s.on_alloc(size));
        // SAFETY: `prefix < total`, so `base + prefix` is inside the allocation.
        Ok(Block::new(unsafe { base.add(prefix) }, size, align))
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        if block.ptr().is_null() {
            return Err(AllocError::InvalidArgument);
        }

        for pool in self.pools.iter().flatten() {
            if pool.owns(block.ptr(), 1) {
                // SAFETY: the pointer is inside this pool's storage; the pool validates
                // slot alignment and occupancy before releasing it.
                let r = unsafe {
                    pool.deallocate(Block::from_raw_parts(block.ptr(), pool.object_size(), block.align()))
                };
                if r.is_ok() {
                    self.with_stats(|s| s.on_free(block.size()));
                }
                return r;
            }
        }

        // Fallback: the contract says `block` came from this allocator, so the prefix
        // is readable. See the module note.
        // SAFETY: per the contract, the prefix precedes a live fallback block.
        let mut h: FallbackHeader =
            unsafe { ptr::read_unaligned(block.ptr().sub(FALLBACK_PREFIX) as *const FallbackHeader) };
        if h.magic != FALLBACK_MAGIC {
            return Err(AllocError::InvalidFree);
        }
        if h.size != block.size() {
            return Err(AllocError::InvalidArgument);
        }
        if h.prefix < FALLBACK_PREFIX || h.prefix > FALLBACK_PREFIX + DEFAULT_ALIGNMENT * 256 {
            return Err(AllocError::Corruption);
        }
        h.magic = 0;
        // SAFETY: the header is inside the allocation; poison it so a second release
        // is detected as an invalid free.
        unsafe { ptr::write_unaligned(block.ptr().sub(FALLBACK_PREFIX) as *mut FallbackHeader, h) };
        // SAFETY: `block.ptr() - prefix` is the start of the allocation made in `allocate`.
        unsafe { platform::raw_deallocate(block.ptr().sub(h.prefix)) };
        self.with_stats(|s| s.on_free(block.size()));
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE
            .union(Capabilities::INDIVIDUAL_FREE)
            .union(Capabilities::OWNERSHIP)
            .union(Capabilities::STATISTICS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_selection() {
        assert_eq!(SizeClassAllocator::class_index_for(1), 0);
        assert_eq!(SizeClassAllocator::class_index_for(16), 0);
        assert_eq!(SizeClassAllocator::class_index_for(17), 1);
        assert_eq!(SizeClassAllocator::class_index_for(4096), 8);
        assert_eq!(SizeClassAllocator::class_index_for(4097), CLASS_COUNT);
    }

    #[test]
    fn small_requests_use_pools_and_release_them() {
        let s = SizeClassAllocator::create(8).unwrap();
        let a = s.allocate(20, 8).unwrap();
        assert_eq!(s.pool(1).unwrap().slots_in_use(), 1);
        assert_eq!(s.pooled_allocations(), 1);
        // SAFETY: `a` came from `s` and is released once.
        unsafe { s.deallocate(a).unwrap() };
        assert_eq!(s.pool(1).unwrap().slots_in_use(), 0);
    }

    #[test]
    fn falls_back_when_pool_full_and_for_large_blocks() {
        let s = SizeClassAllocator::create(2).unwrap();
        let a = s.allocate(16, 16).unwrap();
        let b = s.allocate(16, 16).unwrap();
        let c = s.allocate(16, 16).unwrap();
        assert_eq!(s.fallback_allocations(), 1);
        assert!(!s.owns_pooled(c.ptr(), 1));
        let big = s.allocate(10_000, 64).unwrap();
        assert_eq!(big.ptr() as usize & 63, 0);
        // SAFETY: each block came from `s` and is released once.
        unsafe {
            s.deallocate(c).unwrap();
            s.deallocate(big).unwrap();
            s.deallocate(a).unwrap();
            s.deallocate(b).unwrap();
        }
        assert_eq!(s.stats().live_allocations(), 0);
    }

    #[test]
    fn double_free_in_pool_is_detected() {
        let s = SizeClassAllocator::create(4).unwrap();
        let a = s.allocate(64, 16).unwrap();
        // SAFETY: the second release is the misuse under test; the pool refuses it.
        unsafe {
            s.deallocate(a).unwrap();
            assert_eq!(s.deallocate(a), Err(AllocError::DoubleFree));
        }
    }

    #[test]
    fn fallback_size_mismatch_is_refused() {
        let s = SizeClassAllocator::create(1).unwrap();
        let big = s.allocate(5000, 16).unwrap();
        // SAFETY: the mismatched size is refused before any release; the block is then released once.
        unsafe {
            let wrong = Block::from_raw_parts(big.ptr(), 4999, 16);
            assert_eq!(s.deallocate(wrong), Err(AllocError::InvalidArgument));
            s.deallocate(big).unwrap();
        }
    }
}
