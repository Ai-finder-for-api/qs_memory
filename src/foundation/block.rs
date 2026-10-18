//! `Block`: a region handed out by an allocator.
//!
//! A block is plain data (a pointer, a size and an alignment), so it is `Copy`.
//! Holding one does not keep the memory alive; the allocator that produced it owns
//! the memory until the block is released with `Allocator::deallocate`.

use core::ptr;

/// A region of memory obtained from an allocator.
///
/// A block with size 0 is the canonical empty result of a zero-sized request. Its
/// pointer is null.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Block {
    ptr: *mut u8,
    size: usize,
    align: usize,
}

// SAFETY: a `Block` is an address and a length. Sending it between threads does not
// by itself access memory; memory access is governed by the allocator contract.
unsafe impl Send for Block {}
// SAFETY: see `Send` above; a shared `&Block` only reads plain fields.
unsafe impl Sync for Block {}

impl Block {
    /// The empty block with the given alignment.
    pub const fn empty(align: usize) -> Self {
        Block { ptr: ptr::null_mut(), size: 0, align }
    }

    /// Internal constructor. Callers must have established the allocator invariants.
    pub(crate) const fn new(ptr: *mut u8, size: usize, align: usize) -> Self {
        Block { ptr, size, align }
    }

    /// Builds a block from raw parts, for interop and tests.
    ///
    /// # Safety
    /// Passing the result to `Allocator::deallocate` is only sound when `ptr`, `size`
    /// and `align` describe a live block that allocator returned.
    pub unsafe fn from_raw_parts(ptr: *mut u8, size: usize, align: usize) -> Self {
        Block { ptr, size, align }
    }

    /// Start of the block (null when empty).
    pub const fn ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// Usable bytes.
    pub const fn size(&self) -> usize {
        self.size
    }

    /// Alignment the block was requested with.
    pub const fn align(&self) -> usize {
        self.align
    }

    /// True for zero-sized blocks.
    pub const fn is_empty(&self) -> bool {
        self.size == 0
    }

    /// True when `[p, p + n)` lies entirely inside this block.
    pub fn contains(&self, p: *const u8, n: usize) -> bool {
        if self.is_empty() || p.is_null() {
            return false;
        }
        let base = self.ptr as usize;
        let addr = p as usize;
        if addr < base {
            return false;
        }
        let offset = addr - base;
        if offset > self.size {
            return false;
        }
        n <= self.size - offset
    }

    /// Views the block as a mutable byte slice.
    ///
    /// # Safety
    /// The block must be live, and no other reference to its bytes may be in use
    /// for the returned lifetime.
    pub unsafe fn as_mut_slice<'b>(self) -> &'b mut [u8] {
        if self.size == 0 {
            return &mut [];
        }
        // SAFETY: the caller guarantees the block is live and exclusively accessed.
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.size) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_block_contains_nothing() {
        let b = Block::empty(8);
        assert!(b.is_empty());
        assert!(b.ptr().is_null());
        assert!(!b.contains(core::ptr::null(), 0));
    }

    #[test]
    fn contains_respects_bounds() {
        let mut buf = [0u8; 32];
        let base = buf.as_mut_ptr();
        let b = Block::new(base, 32, 1);
        // SAFETY: indices stay inside `buf`.
        unsafe {
            assert!(b.contains(base, 32));
            assert!(b.contains(base.add(8), 24));
            assert!(!b.contains(base.add(8), 25));
            assert!(!b.contains(base.add(32), 1));
        }
    }
}
