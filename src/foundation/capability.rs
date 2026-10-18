//! Capability bits describing what an allocator supports.
//!
//! Callers check capabilities instead of guessing. An operation that an allocator
//! does not support returns `AllocError::NotSupported`.

use core::ops::BitOr;

/// A set of allocator capabilities.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Capabilities(u32);

impl Capabilities {
    /// No capabilities.
    pub const NONE: Self = Self(0);
    /// `allocate` produces blocks.
    pub const ALLOCATE: Self = Self(1 << 0);
    /// Any live block may be released on its own.
    pub const INDIVIDUAL_FREE: Self = Self(1 << 1);
    /// Blocks can be resized while preserving contents.
    pub const REALLOCATE: Self = Self(1 << 2);
    /// `owns` answers membership queries.
    pub const OWNERSHIP: Self = Self(1 << 3);
    /// Everything can be released at once.
    pub const RESET: Self = Self(1 << 4);
    /// Checkpoints can restore an earlier state.
    pub const ROLLBACK: Self = Self(1 << 5);
    /// Blocks must be released in reverse allocation order.
    pub const STACK_ORDER: Self = Self(1 << 6);
    /// Concurrent calls are safe.
    pub const THREAD_SAFE: Self = Self(1 << 7);
    /// `stats` reports meaningful counters.
    pub const STATISTICS: Self = Self(1 << 8);

    /// Raw bit representation.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Union of two sets.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Intersection of two sets.
    pub const fn intersection(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// True when every bit of `other` is set in `self`. An empty `other` is never contained.
    pub const fn contains(self, other: Self) -> bool {
        other.0 != 0 && (self.0 & other.0) == other.0
    }
}

impl BitOr for Capabilities {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_algebra() {
        let s = Capabilities::ALLOCATE | Capabilities::RESET;
        assert!(s.contains(Capabilities::RESET));
        assert!(!s.contains(Capabilities::THREAD_SAFE));
        assert!(!s.contains(Capabilities::NONE));
        assert_eq!(
            s.intersection(Capabilities::RESET | Capabilities::THREAD_SAFE),
            Capabilities::RESET
        );
    }
}
