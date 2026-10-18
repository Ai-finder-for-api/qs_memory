//! Bulk memory helpers (feature 46). Fill, zero and copy work a machine word at a time when the addresses
//! allow it, and fall back to bytes at the unaligned edges. `copy` detects overlap and behaves like memmove.

/// True when `p` is a multiple of `align` (a power of two).
pub fn is_aligned(p: *const u8, align: usize) -> bool {
    align.is_power_of_two() && (p as usize) & (align - 1) == 0
}

/// Sets `len` bytes at `dst` to `byte`.
///
/// # Safety
/// `[dst, dst + len)` must be writable.
pub unsafe fn fill(dst: *mut u8, byte: u8, len: usize) {
    let word = u64::from_ne_bytes([byte; 8]);
    let mut i = 0;
    while i < len && (dst as usize + i) % 8 != 0 {
        // SAFETY: `i < len`, and the range is writable by the caller's contract.
        unsafe { *dst.add(i) = byte };
        i += 1;
    }
    while i + 8 <= len {
        // SAFETY: `dst + i` is 8-byte aligned and eight bytes remain inside the range.
        unsafe { (dst.add(i) as *mut u64).write(word) };
        i += 8;
    }
    while i < len {
        // SAFETY: `i < len`, and the range is writable by the caller's contract.
        unsafe { *dst.add(i) = byte };
        i += 1;
    }
}

/// Sets `len` bytes at `dst` to zero.
///
/// # Safety
/// `[dst, dst + len)` must be writable.
pub unsafe fn zero(dst: *mut u8, len: usize) {
    // SAFETY: forwarded from this function's contract.
    unsafe { fill(dst, 0, len) }
}

/// Copies `len` bytes from `src` to `dst`. The ranges may overlap.
///
/// # Safety
/// `[src, src + len)` must be readable and `[dst, dst + len)` writable.
pub unsafe fn copy(dst: *mut u8, src: *const u8, len: usize) {
    let (d, s) = (dst as usize, src as usize);
    let overlaps = d < s + len && s < d + len;
    if overlaps || len < 16 || d % 8 != s % 8 {
        // SAFETY: the caller's contract covers both ranges; `ptr::copy` handles overlap.
        unsafe { core::ptr::copy(src, dst, len) };
        return;
    }
    let mut i = 0;
    while i < len && (d + i) % 8 != 0 {
        // SAFETY: `i < len`, and both ranges are valid by the caller's contract.
        unsafe { *dst.add(i) = *src.add(i) };
        i += 1;
    }
    while i + 8 <= len {
        // SAFETY: both `dst + i` and `src + i` are 8-byte aligned (equal misalignment, aligned head) and eight
        // bytes remain inside both ranges.
        unsafe { (dst.add(i) as *mut u64).write((src.add(i) as *const u64).read()) };
        i += 8;
    }
    while i < len {
        // SAFETY: `i < len`, and both ranges are valid by the caller's contract.
        unsafe { *dst.add(i) = *src.add(i) };
        i += 1;
    }
}
