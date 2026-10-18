//! Bump allocator over one region.
//!
//! - `allocate` is O(1): align the top, check capacity, advance the top.
//! - Individual blocks cannot be freed (`NotSupported`). Memory is reclaimed in bulk
//!   with `reset`, or back to a checkpoint with `rollback`.
//! - Checkpoints are scope markers: roll back in LIFO order. A checkpoint records the
//!   top, live-block count and live-byte count; rollback restores all three. A
//!   checkpoint above the current top is `InvalidCheckpoint`; one taken before a
//!   `reset` is `StaleCheckpoint`. An arena cannot tell two histories that reach the
//!   same top apart; use `StackAllocator` when exact history validation is required.
//! - Not `Sync`. Objects placed in an arena are not dropped by it.
//!
//! An arena either owns its storage (`Arena::new`) or borrows caller storage
//! (`Arena::from_slice`).

use core::cell::Cell;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ptr;

use crate::foundation::align::{align_up, is_valid_alignment};
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::{DEBUG_CHECKS, DEFAULT_ALIGNMENT, FREED_BYTE, MAX_BLOCK_SIZE, UNINIT_BYTE};
use crate::foundation::error::{AllocError, AllocResult};
use crate::foundation::stats::Stats;
use crate::platform;

/// A position an arena can be rolled back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    top: usize,
    live_blocks: u64,
    live_bytes: u64,
    generation: u32,
}

/// Bump allocator over one contiguous region.
pub struct Arena<'a> {
    base: *mut u8,
    capacity: usize,
    top: Cell<usize>,
    high_water: Cell<usize>,
    live_blocks: Cell<u64>,
    generation: Cell<u32>,
    stats: Cell<Stats>,
    owned: bool,
    _storage: PhantomData<&'a mut [MaybeUninit<u8>]>,
}

// SAFETY: an arena owns or exclusively borrows its region and has no thread affinity;
// moving it to another thread is sound. It is not `Sync` (it contains `Cell`s).
unsafe impl Send for Arena<'_> {}

impl<'a> Arena<'a> {
    fn empty() -> Self {
        Arena {
            base: ptr::null_mut(),
            capacity: 0,
            top: Cell::new(0),
            high_water: Cell::new(0),
            live_blocks: Cell::new(0),
            generation: Cell::new(0),
            stats: Cell::new(Stats::default()),
            owned: false,
            _storage: PhantomData,
        }
    }

    /// An arena over caller-provided storage. The storage is borrowed for `'a`.
    pub fn from_slice(storage: &'a mut [MaybeUninit<u8>]) -> Self {
        let mut a = Self::empty();
        a.base = storage.as_mut_ptr() as *mut u8;
        a.capacity = storage.len();
        a.stats.set(Stats { capacity_bytes: storage.len() as u64, ..Stats::default() });
        a
    }

    fn with_stats<R>(&self, f: impl FnOnce(&mut Stats) -> R) -> R {
        let mut s = self.stats.get();
        let r = f(&mut s);
        self.stats.set(s);
        r
    }

    fn fail(&self, e: AllocError) -> AllocError {
        self.with_stats(|s| s.on_failure());
        e
    }

    /// Bytes handed out so far, including alignment padding.
    pub fn used(&self) -> usize {
        self.top.get()
    }

    /// Bytes still available.
    pub fn remaining(&self) -> usize {
        self.capacity - self.top.get()
    }

    /// Total capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Largest value `used` has reached.
    pub fn high_water(&self) -> usize {
        self.high_water.get()
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    /// True when `[p, p + n)` lies inside the arena's storage.
    pub fn owns(&self, p: *const u8, n: usize) -> bool {
        if self.base.is_null() || p.is_null() {
            return false;
        }
        let base = self.base as usize;
        let addr = p as usize;
        if addr < base {
            return false;
        }
        let offset = addr - base;
        if offset > self.capacity {
            return false;
        }
        n <= self.capacity - offset
    }

    /// Captures the current state.
    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            top: self.top.get(),
            live_blocks: self.live_blocks.get(),
            live_bytes: self.stats.get().bytes_live,
            generation: self.generation.get(),
        }
    }

    /// Restores the state captured by `cp`.
    pub fn rollback(&self, cp: Checkpoint) -> AllocResult<()> {
        if cp.generation != self.generation.get() {
            return Err(AllocError::StaleCheckpoint);
        }
        let top = self.top.get();
        if cp.top > top
            || cp.live_blocks > self.live_blocks.get()
            || cp.live_bytes > self.stats.get().bytes_live
        {
            return Err(AllocError::InvalidCheckpoint);
        }
        if DEBUG_CHECKS && top > cp.top {
            // SAFETY: `top <= capacity`, so `[cp.top, top)` lies inside the storage;
            // `top > cp.top >= 0` implies the arena has storage.
            unsafe { ptr::write_bytes(self.base.add(cp.top), FREED_BYTE, top - cp.top) };
        }
        let released = self.live_blocks.get() - cp.live_blocks;
        self.with_stats(|s| {
            s.deallocations += released;
            s.bytes_live = cp.live_bytes;
        });
        self.live_blocks.set(cp.live_blocks);
        self.top.set(cp.top);
        Ok(())
    }

    /// Releases everything. Blocks handed out before this call become invalid.
    pub fn reset(&self) {
        let top = self.top.get();
        if DEBUG_CHECKS && top > 0 {
            // SAFETY: `top <= capacity`, so `[0, top)` lies inside the storage.
            unsafe { ptr::write_bytes(self.base, FREED_BYTE, top) };
        }
        let live = self.live_blocks.get();
        self.with_stats(|s| {
            s.deallocations += live;
            s.bytes_live = 0;
        });
        self.live_blocks.set(0);
        self.top.set(0);
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    fn empty_owned() -> Arena<'static> {
        Arena::empty()
    }
}

impl Arena<'static> {
    /// An arena that owns `capacity` bytes of storage.
    pub fn new(capacity: usize) -> AllocResult<Self> {
        if capacity == 0 {
            return Ok(Self::empty_owned());
        }
        if capacity > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }
        let p = platform::raw_allocate(capacity, DEFAULT_ALIGNMENT);
        if p.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        let mut a = Self::empty_owned();
        a.base = p;
        a.capacity = capacity;
        a.owned = true;
        a.stats.set(Stats { capacity_bytes: capacity as u64, ..Stats::default() });
        Ok(a)
    }
}

impl Drop for Arena<'_> {
    fn drop(&mut self) {
        if self.owned && !self.base.is_null() {
            // SAFETY: an owned arena's base came from `platform::raw_allocate` in `new`
            // and is released only here.
            unsafe { platform::raw_deallocate(self.base) };
        }
    }
}

impl Allocator for Arena<'_> {
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
        if self.capacity == 0 {
            return Err(self.fail(AllocError::CapacityExhausted));
        }
        let base_addr = self.base as usize;
        let aligned = match base_addr
            .checked_add(self.top.get())
            .and_then(|a| align_up(a, align))
        {
            Some(a) => a,
            None => return Err(self.fail(AllocError::Overflow)),
        };
        let offset = aligned - base_addr;
        if offset > self.capacity || size > self.capacity - offset {
            return Err(self.fail(AllocError::CapacityExhausted));
        }
        // SAFETY: `offset <= capacity`, so the result is in (or one past) the storage;
        // the block `[offset, offset + size)` fits by the check above.
        let p = unsafe { self.base.add(offset) };
        if DEBUG_CHECKS {
            // SAFETY: the block lies inside the storage (checked above).
            unsafe { ptr::write_bytes(p, UNINIT_BYTE, size) };
        }
        let new_top = offset + size;
        self.top.set(new_top);
        if new_top > self.high_water.get() {
            self.high_water.set(new_top);
        }
        self.live_blocks.set(self.live_blocks.get() + 1);
        self.with_stats(|s| s.on_alloc(size));
        Ok(Block::new(p, size, align))
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        if block.ptr().is_null() {
            return Err(AllocError::InvalidArgument);
        }
        if !self.owns(block.ptr(), block.size()) {
            return Err(AllocError::NotOwned);
        }
        Err(AllocError::NotSupported)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE
            .union(Capabilities::RESET)
            .union(Capabilities::ROLLBACK)
            .union(Capabilities::OWNERSHIP)
            .union(Capabilities::STATISTICS)
    }
}

/// RAII scope: rolls the arena back to its state at creation unless dismissed.
#[must_use = "the scope rolls the arena back when dropped"]
pub struct ArenaScope<'s, 'a> {
    arena: &'s Arena<'a>,
    checkpoint: Checkpoint,
    active: bool,
}

impl<'s, 'a> ArenaScope<'s, 'a> {
    /// Starts a scope that will roll `arena` back when dropped.
    pub fn new(arena: &'s Arena<'a>) -> Self {
        Self { arena, checkpoint: arena.checkpoint(), active: true }
    }

    /// Keeps everything allocated inside the scope.
    pub fn dismiss(mut self) {
        self.active = false;
    }
}

impl Drop for ArenaScope<'_, '_> {
    fn drop(&mut self) {
        if self.active {
            let _ = self.arena.rollback(self.checkpoint);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage<const N: usize>() -> [MaybeUninit<u8>; N] {
        [MaybeUninit::uninit(); N]
    }

    #[test]
    fn allocates_aligned_and_tracks_used() {
        let mut buf = storage::<4096>();
        let a = Arena::from_slice(&mut buf);
        for align in [1usize, 8, 64, 256] {
            let b = a.allocate(33, align).unwrap();
            assert_eq!(b.ptr() as usize & (align - 1), 0);
        }
        assert!(a.used() > 0);
        assert_eq!(a.high_water(), a.used());
    }

    #[test]
    fn exhaustion_and_zero_size() {
        let mut buf = storage::<128>();
        let a = Arena::from_slice(&mut buf);
        assert!(a.allocate(0, 8).unwrap().is_empty());
        assert_eq!(a.used(), 0);
        a.allocate(100, 1).unwrap();
        assert_eq!(a.allocate(64, 1).unwrap_err(), AllocError::CapacityExhausted);
        assert_eq!(a.allocate(8, 3).unwrap_err(), AllocError::InvalidAlignment);
        assert_eq!(a.stats().failed_allocations, 2);
    }

    #[test]
    fn individual_free_is_not_supported() {
        let mut buf = storage::<256>();
        let a = Arena::from_slice(&mut buf);
        let b = a.allocate(32, 8).unwrap();
        // SAFETY: `b` came from `a`; the call is expected to refuse it, so it stays live.
        unsafe {
            assert_eq!(a.deallocate(b), Err(AllocError::NotSupported));
            assert_eq!(a.deallocate(Block::empty(8)), Ok(()));
        }
        let outside = 0u8;
        // SAFETY: a foreign pointer is only compared by address; nothing is read.
        unsafe {
            assert_eq!(
                a.deallocate(Block::from_raw_parts(&outside as *const u8 as *mut u8, 4, 1)),
                Err(AllocError::NotOwned)
            );
        }
    }

    #[test]
    fn reset_reclaims_and_stales_checkpoints() {
        let mut buf = storage::<512>();
        let a = Arena::from_slice(&mut buf);
        a.allocate(200, 8).unwrap();
        let cp = a.checkpoint();
        a.reset();
        assert_eq!(a.used(), 0);
        assert_eq!(a.stats().live_allocations(), 0);
        a.allocate(200, 8).unwrap();
        assert_eq!(a.rollback(cp), Err(AllocError::StaleCheckpoint));
    }

    #[test]
    fn nested_checkpoints_restore_state() {
        let mut buf = storage::<2048>();
        let a = Arena::from_slice(&mut buf);
        a.allocate(16, 8).unwrap();
        let outer = a.checkpoint();
        a.allocate(32, 8).unwrap();
        let inner = a.checkpoint();
        a.allocate(64, 8).unwrap();
        a.rollback(inner).unwrap();
        assert_eq!(a.used(), inner.top);
        a.rollback(outer).unwrap();
        assert_eq!(a.stats().live_allocations(), 1);
        assert_eq!(a.stats().bytes_live, 16);
    }

    #[test]
    fn scope_rolls_back_unless_dismissed() {
        let mut buf = storage::<1024>();
        let a = Arena::from_slice(&mut buf);
        a.allocate(24, 8).unwrap();
        let before = a.used();
        {
            let _scope = ArenaScope::new(&a);
            a.allocate(100, 16).unwrap();
        }
        assert_eq!(a.used(), before);
        {
            let scope = ArenaScope::new(&a);
            a.allocate(50, 8).unwrap();
            scope.dismiss();
        }
        assert!(a.used() > before);
    }

    #[test]
    fn owned_arena_releases_its_storage() {
        let a = Arena::new(1024).unwrap();
        let b = a.allocate(100, 64).unwrap();
        assert_eq!(b.ptr() as usize & 63, 0);
        assert_eq!(a.capacity(), 1024);
    }
}
