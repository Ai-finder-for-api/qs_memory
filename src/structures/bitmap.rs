//! Fixed-size bit set backed by raw storage (no heap containers).
//!
//! Used for occupancy and page-commit tracking. Mutators take `&self` and write through
//! the raw storage, so a structure that holds a `Bitmap` does not need `&mut self` to
//! update it. The bitmap is not `Sync`.

use core::ptr;

use crate::config::MAX_BLOCK_SIZE;
use crate::foundation::error::{AllocError, AllocResult};
use crate::platform;

/// Returned by searches that find nothing.
pub const NPOS: usize = usize::MAX;

/// A bit set of fixed length.
pub struct Bitmap {
    words: *mut u64,
    bits: usize,
}

// SAFETY: the bitmap owns its storage and has no thread affinity; it is not `Sync`.
unsafe impl Send for Bitmap {}

impl Bitmap {
    /// An all-clear bitmap of `bits` bits.
    pub fn create(bits: usize) -> AllocResult<Self> {
        if bits == 0 {
            return Ok(Bitmap { words: ptr::null_mut(), bits: 0 });
        }
        let nwords = bits.div_ceil(64);
        if nwords > MAX_BLOCK_SIZE / core::mem::size_of::<u64>() {
            return Err(AllocError::Overflow);
        }
        let words = platform::raw_allocate(nwords * core::mem::size_of::<u64>(), core::mem::align_of::<u64>())
            as *mut u64;
        if words.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        // SAFETY: `words` holds `nwords` u64 values.
        unsafe { ptr::write_bytes(words, 0, nwords) };
        Ok(Bitmap { words, bits })
    }

    /// Number of bits.
    pub fn len(&self) -> usize {
        self.bits
    }

    /// True for a zero-length bitmap.
    pub fn is_empty(&self) -> bool {
        self.bits == 0
    }

    /// Reads bit `i`. Panics in debug builds when `i` is out of range.
    pub fn test(&self, i: usize) -> bool {
        debug_assert!(i < self.bits, "bitmap index out of range");
        // SAFETY: `i < bits`, so word `i / 64` exists.
        unsafe { (*self.words.add(i / 64) >> (i % 64)) & 1 == 1 }
    }

    /// Sets bit `i`.
    pub fn set(&self, i: usize) {
        debug_assert!(i < self.bits, "bitmap index out of range");
        // SAFETY: `i < bits`, so word `i / 64` exists.
        unsafe { *self.words.add(i / 64) |= 1u64 << (i % 64) };
    }

    /// Clears bit `i`.
    pub fn reset(&self, i: usize) {
        debug_assert!(i < self.bits, "bitmap index out of range");
        // SAFETY: `i < bits`, so word `i / 64` exists.
        unsafe { *self.words.add(i / 64) &= !(1u64 << (i % 64)) };
    }

    /// Sets `[begin, end)`. Returns how many bits changed from clear to set.
    pub fn set_range(&self, begin: usize, end: usize) -> usize {
        let mut changed = 0;
        for i in begin..end.min(self.bits) {
            if !self.test(i) {
                self.set(i);
                changed += 1;
            }
        }
        changed
    }

    /// Clears `[begin, end)`. Returns how many bits changed from set to clear.
    pub fn clear_range(&self, begin: usize, end: usize) -> usize {
        let mut changed = 0;
        for i in begin..end.min(self.bits) {
            if self.test(i) {
                self.reset(i);
                changed += 1;
            }
        }
        changed
    }

    /// Number of set bits.
    pub fn count(&self) -> usize {
        let nwords = self.bits.div_ceil(64);
        let mut n = 0;
        for w in 0..nwords {
            // SAFETY: `w < nwords`, so the word exists.
            n += unsafe { (*self.words.add(w)).count_ones() } as usize;
        }
        n
    }

    /// First clear bit at or after `from`, or `NPOS`.
    pub fn find_first_clear(&self, from: usize) -> usize {
        (from..self.bits).find(|&i| !self.test(i)).unwrap_or(NPOS)
    }

    /// First set bit at or after `from`, or `NPOS`.
    pub fn find_first_set(&self, from: usize) -> usize {
        (from..self.bits).find(|&i| self.test(i)).unwrap_or(NPOS)
    }

    /// First index that starts a run of `len` clear bits, or `NPOS`.
    pub fn find_clear_run(&self, len: usize) -> usize {
        if len == 0 || len > self.bits {
            return NPOS;
        }
        let mut run = 0;
        for i in 0..self.bits {
            run = if self.test(i) { 0 } else { run + 1 };
            if run == len {
                return i + 1 - len;
            }
        }
        NPOS
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        if !self.words.is_null() {
            // SAFETY: `words` came from `raw_allocate` in `create` and is released once.
            unsafe { platform::raw_deallocate(self.words as *mut u8) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_reset_count_and_ranges() {
        let b = Bitmap::create(200).unwrap();
        assert_eq!(b.len(), 200);
        b.set(5);
        b.set(199);
        assert!(b.test(5) && b.test(199) && !b.test(6));
        assert_eq!(b.count(), 2);
        assert_eq!(b.set_range(10, 70), 60);
        assert_eq!(b.set_range(50, 80), 10);
        assert_eq!(b.clear_range(0, 20), 11);
        assert_eq!(b.find_first_set(0), 20);
        assert_eq!(b.find_first_clear(10), 10);
    }

    #[test]
    fn clear_runs() {
        let b = Bitmap::create(64).unwrap();
        b.set_range(0, 10);
        b.set_range(12, 20);
        assert_eq!(b.find_clear_run(2), 10);
        assert_eq!(b.find_clear_run(3), 20);
        assert_eq!(b.find_clear_run(64), NPOS);
        assert_eq!(b.find_clear_run(0), NPOS);
    }

    #[test]
    fn empty_bitmap() {
        let b = Bitmap::create(0).unwrap();
        assert!(b.is_empty());
        assert_eq!(b.find_first_clear(0), NPOS);
    }
}
