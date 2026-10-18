//! Scratch memory: one region used from both ends.
//!
//! Front allocations grow upward from the start; back allocations grow downward from the
//! end. Each end is rolled back independently with a `Mark`. A generation counter that
//! `reset` advances invalidates older marks.
//!
//! Not an `Allocator` (two entry points), and not `Sync`.

use core::cell::Cell;
use core::marker::PhantomData;
use core::mem::MaybeUninit;
use core::ptr;

use crate::foundation::align::{align_up, is_valid_alignment};
use crate::foundation::block::Block;
use crate::config::{DEFAULT_ALIGNMENT, MAX_BLOCK_SIZE};
use crate::foundation::error::{AllocError, AllocResult};
use crate::platform;

/// A position both ends can be rolled back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    front: usize,
    back: usize,
    generation: u32,
}

/// Region used from both ends.
pub struct DoubleEndedArena<'a> {
    base: *mut u8,
    capacity: usize,
    front: Cell<usize>,
    back: Cell<usize>,
    generation: Cell<u32>,
    owned: bool,
    _storage: PhantomData<&'a mut [MaybeUninit<u8>]>,
}

// SAFETY: the arena owns or exclusively borrows its region and has no thread affinity.
unsafe impl Send for DoubleEndedArena<'_> {}

impl<'a> DoubleEndedArena<'a> {
    fn empty() -> Self {
        DoubleEndedArena {
            base: ptr::null_mut(),
            capacity: 0,
            front: Cell::new(0),
            back: Cell::new(0),
            generation: Cell::new(0),
            owned: false,
            _storage: PhantomData,
        }
    }

    /// An arena over caller-provided storage, borrowed for `'a`.
    pub fn from_slice(storage: &'a mut [MaybeUninit<u8>]) -> Self {
        let mut a = Self::empty();
        a.base = storage.as_mut_ptr() as *mut u8;
        a.capacity = storage.len();
        a
    }

    /// Total capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Bytes used from the front.
    pub fn front_used(&self) -> usize {
        self.front.get()
    }

    /// Bytes used from the back.
    pub fn back_used(&self) -> usize {
        self.back.get()
    }

    /// Bytes available between the two ends.
    pub fn free_bytes(&self) -> usize {
        self.capacity - self.front.get() - self.back.get()
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
        if offset >= self.capacity {
            return false;
        }
        n <= self.capacity - offset
    }

    /// Captures both ends.
    pub fn mark(&self) -> Mark {
        Mark { front: self.front.get(), back: self.back.get(), generation: self.generation.get() }
    }

    /// Restores both ends to `m`.
    pub fn rollback(&self, m: Mark) -> AllocResult<()> {
        if m.generation != self.generation.get() {
            return Err(AllocError::StaleCheckpoint);
        }
        if m.front > self.front.get() || m.back > self.back.get() {
            return Err(AllocError::InvalidCheckpoint);
        }
        self.front.set(m.front);
        self.back.set(m.back);
        Ok(())
    }

    /// Releases everything from both ends.
    pub fn reset(&self) {
        self.front.set(0);
        self.back.set(0);
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    /// Allocates from the front (upward).
    pub fn allocate_front(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !is_valid_alignment(align) {
            return Err(AllocError::InvalidAlignment);
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        if size > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }
        if self.capacity == 0 {
            return Err(AllocError::CapacityExhausted);
        }
        let base = self.base as usize;
        let aligned = base
            .checked_add(self.front.get())
            .and_then(|a| align_up(a, align))
            .ok_or(AllocError::Overflow)?;
        let start = aligned - base;
        if start > self.capacity - self.back.get() || size > self.capacity - self.back.get() - start {
            return Err(AllocError::CapacityExhausted);
        }
        self.front.set(start + size);
        // SAFETY: `start <= capacity`, so the block lies inside the storage.
        Ok(Block::new(unsafe { self.base.add(start) }, size, align))
    }

    /// Allocates from the back (downward). The block ends at an aligned address.
    pub fn allocate_back(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !is_valid_alignment(align) {
            return Err(AllocError::InvalidAlignment);
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        if size > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }
        if self.capacity == 0 {
            return Err(AllocError::CapacityExhausted);
        }
        let base = self.base as usize;
        let limit = self.capacity - self.back.get(); // offset of the lowest used back byte
        if limit < size {
            return Err(AllocError::CapacityExhausted);
        }
        let candidate = base + limit - size;
        let aligned = candidate & !(align - 1);
        let start = aligned - base;
        if start < self.front.get() {
            return Err(AllocError::CapacityExhausted);
        }
        self.back.set(self.capacity - start);
        // SAFETY: `start` lies in `[front, capacity)`, so the block is inside the storage.
        Ok(Block::new(unsafe { self.base.add(start) }, size, align))
    }
}

impl DoubleEndedArena<'static> {
    /// An arena that owns `capacity` bytes of storage.
    pub fn new(capacity: usize) -> AllocResult<Self> {
        if capacity == 0 {
            return Ok(Self::empty());
        }
        if capacity > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }
        let p = platform::raw_allocate(capacity, DEFAULT_ALIGNMENT);
        if p.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        let mut a = Self::empty();
        a.base = p;
        a.capacity = capacity;
        a.owned = true;
        Ok(a)
    }
}

impl Drop for DoubleEndedArena<'_> {
    fn drop(&mut self) {
        if self.owned && !self.base.is_null() {
            // SAFETY: an owned arena's base came from `platform::raw_allocate` in `new`.
            unsafe { platform::raw_deallocate(self.base) };
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
    fn front_and_back_do_not_collide() {
        let mut buf = storage::<256>();
        let d = DoubleEndedArena::from_slice(&mut buf);
        let f = d.allocate_front(40, 8).unwrap();
        let b = d.allocate_back(40, 8).unwrap();
        assert!(f.ptr() < b.ptr());
        assert_eq!(b.ptr() as usize & 7, 0);
        assert_eq!(d.free_bytes(), 256 - 80);
        assert_eq!(d.allocate_front(200, 1).unwrap_err(), AllocError::CapacityExhausted);
        let aligned = d.allocate_back(16, 64).unwrap();
        assert_eq!(aligned.ptr() as usize & 63, 0);
        assert!(aligned.ptr() > f.ptr());
    }

    #[test]
    fn marks_roll_back_each_end_and_reset_stales_them() {
        let mut buf = storage::<512>();
        let d = DoubleEndedArena::from_slice(&mut buf);
        d.allocate_front(16, 1).unwrap();
        let m = d.mark();
        d.allocate_front(32, 1).unwrap();
        d.allocate_back(64, 1).unwrap();
        d.rollback(m).unwrap();
        assert_eq!(d.front_used(), 16);
        assert_eq!(d.back_used(), 0);
        d.reset();
        assert_eq!(d.rollback(m), Err(AllocError::StaleCheckpoint));
    }

    #[test]
    fn owned_arena() {
        let d = DoubleEndedArena::new(1024).unwrap();
        let b = d.allocate_back(100, 16).unwrap();
        assert!(d.owns(b.ptr(), 100));
        assert_eq!(d.capacity(), 1024);
    }
}
