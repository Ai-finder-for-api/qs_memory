//! Alignment and overflow-checked size arithmetic. All functions are pure.

use crate::config::MAX_ALIGNMENT;

/// True when `v` is a non-zero power of two.
#[inline]
pub const fn is_power_of_two(v: usize) -> bool {
    v != 0 && (v & (v - 1)) == 0
}

/// True when `a` is a power of two no larger than `MAX_ALIGNMENT`.
#[inline]
pub const fn is_valid_alignment(a: usize) -> bool {
    is_power_of_two(a) && a <= MAX_ALIGNMENT
}

/// Rounds `v` up to a multiple of `a`. `a` must be a power of two.
/// Returns `None` when the result would overflow.
#[inline]
pub const fn align_up(v: usize, a: usize) -> Option<usize> {
    match v.checked_add(a - 1) {
        Some(biased) => Some(biased & !(a - 1)),
        None => None,
    }
}

/// Rounds `v` down to a multiple of `a`. `a` must be a power of two.
#[inline]
pub const fn align_down(v: usize, a: usize) -> usize {
    v & !(a - 1)
}

/// True when the address `addr` is a multiple of `a`. `a` must be a power of two.
#[inline]
pub const fn is_aligned_addr(addr: usize, a: usize) -> bool {
    addr & (a - 1) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_of_two_and_validity() {
        assert!(!is_power_of_two(0));
        assert!(is_power_of_two(1));
        assert!(!is_power_of_two(3));
        assert!(is_valid_alignment(4096));
        assert!(!is_valid_alignment(8192));
        assert!(!is_valid_alignment(0));
    }

    #[test]
    fn rounding_and_overflow() {
        assert_eq!(align_up(0, 16), Some(0));
        assert_eq!(align_up(1, 16), Some(16));
        assert_eq!(align_up(17, 16), Some(32));
        assert_eq!(align_up(usize::MAX - 3, 16), None);
        assert_eq!(align_down(31, 16), 16);
        assert!(is_aligned_addr(64, 64));
        assert!(!is_aligned_addr(65, 64));
    }
}
