//! Fixed-size object pool.
//!
//! Every slot holds exactly one object-sized block. Free slots form an intrusive list
//! threaded through the slot storage. An occupancy bitmap (one bit per slot) detects
//! double frees and validates every release. `allocate` and `deallocate` are O(1).
//!
//! Requests must not exceed the object size, and alignment must not exceed the slot
//! alignment. Returned blocks report `size == object_size`.
//! Not `Sync`.

use core::cell::Cell;
use core::mem::{align_of, size_of};
use core::ptr;

use crate::foundation::align::{align_up, is_valid_alignment};
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::{DEBUG_CHECKS, FREED_BYTE, MAX_BLOCK_SIZE, UNINIT_BYTE};
use crate::foundation::error::{AllocError, AllocResult};
use crate::foundation::stats::Stats;
use crate::platform;

const NONE: usize = usize::MAX;

/// Fixed-size slots with exact occupancy tracking.
#[derive(Debug)]
pub struct PoolAllocator {
    storage: *mut u8,
    bitmap: *mut u64,
    count: usize,
    object_size: usize,
    slot_size: usize,
    slot_align: usize,
    free_head: Cell<usize>,
    in_use: Cell<usize>,
    stats: Cell<Stats>,
}

// SAFETY: the pool owns its storage and has no thread affinity; it is not `Sync`.
unsafe impl Send for PoolAllocator {}

impl PoolAllocator {
    /// Creates a pool of `slot_count` slots, each able to hold `object_size` bytes
    /// aligned to `object_align`.
    pub fn create(object_size: usize, object_align: usize, slot_count: usize) -> AllocResult<Self> {
        if object_size == 0 || slot_count == 0 {
            return Err(AllocError::InvalidArgument);
        }
        if !is_valid_alignment(object_align) {
            return Err(AllocError::InvalidAlignment);
        }
        if object_size > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }

        // Each slot must hold a free-list link and be aligned for the object.
        let slot_align = object_align.max(align_of::<usize>());
        let min_slot = object_size.max(size_of::<usize>());
        let slot_size = align_up(min_slot, slot_align).ok_or(AllocError::Overflow)?;
        let total = slot_size.checked_mul(slot_count).ok_or(AllocError::Overflow)?;
        if total > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }

        let storage = platform::raw_allocate(total, slot_align);
        if storage.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        let words = slot_count.div_ceil(64);
        let bitmap = platform::raw_allocate(words * size_of::<u64>(), align_of::<u64>()) as *mut u64;
        if bitmap.is_null() {
            // SAFETY: `storage` came from `raw_allocate` and is released exactly once here.
            unsafe { platform::raw_deallocate(storage) };
            return Err(AllocError::OutOfMemory);
        }
        // SAFETY: `bitmap` holds `words` u64 values.
        unsafe { ptr::write_bytes(bitmap, 0, words) };

        // Thread every slot onto the free list in address order.
        for i in 0..slot_count {
            let next = if i + 1 < slot_count { i + 1 } else { NONE };
            // SAFETY: slot `i` lies inside `storage` and holds at least one usize.
            unsafe {
                ptr::write_unaligned(storage.add(i * slot_size) as *mut usize, next);
            }
        }

        Ok(PoolAllocator {
            storage,
            bitmap,
            count: slot_count,
            object_size,
            slot_size,
            slot_align,
            free_head: Cell::new(0),
            in_use: Cell::new(0),
            stats: Cell::new(Stats {
                capacity_bytes: (object_size as u64) * (slot_count as u64),
                ..Stats::default()
            }),
        })
    }

    /// Size of every object this pool hands out.
    pub fn object_size(&self) -> usize {
        self.object_size
    }

    /// Distance between consecutive slots.
    pub fn slot_size(&self) -> usize {
        self.slot_size
    }

    /// Total number of slots.
    pub fn slot_count(&self) -> usize {
        self.count
    }

    /// Slots currently handed out.
    pub fn slots_in_use(&self) -> usize {
        self.in_use.get()
    }

    /// Slots currently free.
    pub fn slots_free(&self) -> usize {
        self.count - self.in_use.get()
    }

    /// True when slot `index` is currently allocated.
    pub fn is_slot_allocated(&self, index: usize) -> bool {
        if index >= self.count {
            return false;
        }
        // SAFETY: `index < count`, so word `index / 64` is inside the bitmap.
        unsafe { (*self.bitmap.add(index / 64) >> (index % 64)) & 1 != 0 }
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    /// True when `[p, p + n)` lies inside the pool's storage.
    pub fn owns(&self, p: *const u8, n: usize) -> bool {
        if self.storage.is_null() || p.is_null() {
            return false;
        }
        let base = self.storage as usize;
        let addr = p as usize;
        if addr < base {
            return false;
        }
        let offset = addr - base;
        let span = self.count * self.slot_size;
        if offset > span {
            return false;
        }
        n <= span - offset
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

impl Drop for PoolAllocator {
    fn drop(&mut self) {
        if !self.storage.is_null() {
            // SAFETY: `storage` came from `raw_allocate` in `create` and is released once.
            unsafe { platform::raw_deallocate(self.storage) };
        }
        if !self.bitmap.is_null() {
            // SAFETY: `bitmap` came from `raw_allocate` in `create` and is released once.
            unsafe { platform::raw_deallocate(self.bitmap as *mut u8) };
        }
    }
}

impl Allocator for PoolAllocator {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !is_valid_alignment(align) {
            return Err(self.fail(AllocError::InvalidAlignment));
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        if size > self.object_size {
            return Err(self.fail(AllocError::InvalidArgument));
        }
        if align > self.slot_align {
            return Err(self.fail(AllocError::InvalidAlignment));
        }
        let index = self.free_head.get();
        if index == NONE {
            return Err(self.fail(AllocError::CapacityExhausted));
        }
        // SAFETY: a free-list index is always a valid slot of this pool.
        let slot = unsafe { self.storage.add(index * self.slot_size) };
        // SAFETY: the slot's first word holds the next free index, written by `create`
        // or `deallocate`.
        let next = unsafe { ptr::read_unaligned(slot as *const usize) };
        self.free_head.set(next);
        // SAFETY: the bitmap word for `index` exists.
        unsafe { *self.bitmap.add(index / 64) |= 1u64 << (index % 64) };
        self.in_use.set(self.in_use.get() + 1);
        if DEBUG_CHECKS {
            // SAFETY: the slot holds `object_size` usable bytes.
            unsafe { ptr::write_bytes(slot, UNINIT_BYTE, self.object_size) };
        }
        self.with_stats(|s| s.on_alloc(self.object_size));
        Ok(Block::new(slot, self.object_size, align))
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        if block.ptr().is_null() {
            return Err(AllocError::InvalidArgument);
        }
        if block.size() != self.object_size {
            return Err(AllocError::InvalidArgument);
        }
        if !self.owns(block.ptr(), block.size()) {
            return Err(AllocError::NotOwned);
        }
        let offset = block.ptr() as usize - self.storage as usize;
        if offset % self.slot_size != 0 {
            return Err(AllocError::InvalidFree);
        }
        let index = offset / self.slot_size;
        if !self.is_slot_allocated(index) {
            return Err(AllocError::DoubleFree);
        }
        // SAFETY: the bitmap word for `index` exists.
        unsafe { *self.bitmap.add(index / 64) &= !(1u64 << (index % 64)) };
        self.in_use.set(self.in_use.get() - 1);
        // SAFETY: `block` is a slot of this pool, so the whole slot is writable.
        unsafe {
            if DEBUG_CHECKS {
                ptr::write_bytes(block.ptr(), FREED_BYTE, self.slot_size);
            }
            ptr::write_unaligned(block.ptr() as *mut usize, self.free_head.get());
        }
        self.free_head.set(index);
        self.with_stats(|s| s.on_free(self.object_size));
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
    fn allocates_every_slot_then_exhausts() {
        let p = PoolAllocator::create(32, 16, 8).unwrap();
        let mut held = Vec::new();
        for _ in 0..8 {
            let b = p.allocate(32, 16).unwrap();
            assert_eq!(b.ptr() as usize & 15, 0);
            assert!(p.owns(b.ptr(), 32));
            held.push(b);
        }
        assert_eq!(p.slots_in_use(), 8);
        assert_eq!(p.allocate(32, 16).unwrap_err(), AllocError::CapacityExhausted);
        for b in held {
            // SAFETY: each block came from `p` and is released once.
            unsafe { p.deallocate(b).unwrap() };
        }
        assert_eq!(p.slots_in_use(), 0);
    }

    #[test]
    fn released_slots_are_reused_and_double_free_is_caught() {
        let p = PoolAllocator::create(64, 8, 2).unwrap();
        let a = p.allocate(64, 8).unwrap();
        let first = a.ptr();
        // SAFETY: first release of `a`.
        unsafe { p.deallocate(a).unwrap() };
        // SAFETY: the second release is the misuse under test; it is refused before any access.
        unsafe { assert_eq!(p.deallocate(a), Err(AllocError::DoubleFree)) };
        assert_eq!(p.allocate(64, 8).unwrap().ptr(), first);
    }

    #[test]
    fn misaligned_and_wrong_size_frees_are_refused() {
        let p = PoolAllocator::create(32, 8, 4).unwrap();
        let a = p.allocate(32, 8).unwrap();
        // SAFETY: deliberately wrong inputs; each is refused before the pool touches them.
        unsafe {
            let inner = Block::from_raw_parts(a.ptr().add(8), 32, 8);
            assert_eq!(p.deallocate(inner), Err(AllocError::InvalidFree));
            assert_eq!(p.deallocate(Block::from_raw_parts(a.ptr(), 8, 8)), Err(AllocError::InvalidArgument));
            p.deallocate(a).unwrap();
        }
        assert_eq!(p.allocate(33, 8).unwrap_err(), AllocError::InvalidArgument);
    }

    #[test]
    fn create_validates_configuration() {
        assert!(PoolAllocator::create(0, 8, 4).is_err());
        assert!(PoolAllocator::create(16, 8, 0).is_err());
        assert_eq!(PoolAllocator::create(16, 3, 4).unwrap_err(), AllocError::InvalidAlignment);
        assert_eq!(PoolAllocator::create(MAX_BLOCK_SIZE, 8, 4).unwrap_err(), AllocError::Overflow);
    }
}

impl crate::allocators::introspect::LiveBlocks for PoolAllocator {
    fn is_live(&self, block: Block) -> bool {
        if block.ptr().is_null() || block.size() != self.object_size || !self.owns(block.ptr(), block.size()) {
            return false;
        }
        let offset = block.ptr() as usize - self.storage as usize;
        offset % self.slot_size == 0 && self.is_slot_allocated(offset / self.slot_size)
    }
}
