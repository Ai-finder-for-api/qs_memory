//! Error codes. Every fallible operation reports one of these.
//!
//! Numeric values are stable and match the C ABI (`0` means success there).

use core::fmt;

/// Reasons an allocator operation can fail.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AllocError {
    /// Null pointer for a non-empty block, or inconsistent inputs.
    InvalidArgument = 1,
    /// Alignment is not a power of two or exceeds `MAX_ALIGNMENT`.
    InvalidAlignment = 2,
    /// Size arithmetic would wrap, or the size exceeds `MAX_BLOCK_SIZE`.
    Overflow = 3,
    /// The system could not provide memory.
    OutOfMemory = 4,
    /// A bounded allocator has no room left.
    CapacityExhausted = 5,
    /// The pointer is inside this allocator but does not start a live block.
    InvalidFree = 6,
    /// The block was already released.
    DoubleFree = 7,
    /// The checkpoint does not belong to this allocator's current history.
    InvalidCheckpoint = 8,
    /// The checkpoint was invalidated by a reset or by a later rollback.
    StaleCheckpoint = 9,
    /// Stack-order allocator: the block is not the most recent live block.
    OutOfOrderFree = 10,
    /// The operation is not supported by this allocator (see `Capabilities`).
    NotSupported = 11,
    /// A guard or header check failed: memory was overwritten.
    Corruption = 12,
    /// The block lies outside this allocator's storage.
    NotOwned = 13,
}

impl AllocError {
    /// Every error variant, in code order.
    pub const ALL: [AllocError; 13] = [
        AllocError::InvalidArgument,
        AllocError::InvalidAlignment,
        AllocError::Overflow,
        AllocError::OutOfMemory,
        AllocError::CapacityExhausted,
        AllocError::InvalidFree,
        AllocError::DoubleFree,
        AllocError::InvalidCheckpoint,
        AllocError::StaleCheckpoint,
        AllocError::OutOfOrderFree,
        AllocError::NotSupported,
        AllocError::Corruption,
        AllocError::NotOwned,
    ];

    /// Stable numeric code (matches the C ABI).
    pub const fn code(self) -> u8 {
        self as u8
    }

    /// Human-readable description.
    pub const fn as_str(self) -> &'static str {
        match self {
            AllocError::InvalidArgument => "invalid argument",
            AllocError::InvalidAlignment => "invalid alignment",
            AllocError::Overflow => "size overflow",
            AllocError::OutOfMemory => "out of memory",
            AllocError::CapacityExhausted => "capacity exhausted",
            AllocError::InvalidFree => "invalid free",
            AllocError::DoubleFree => "double free",
            AllocError::InvalidCheckpoint => "invalid checkpoint",
            AllocError::StaleCheckpoint => "stale checkpoint",
            AllocError::OutOfOrderFree => "out-of-order free",
            AllocError::NotSupported => "not supported",
            AllocError::Corruption => "memory corruption detected",
            AllocError::NotOwned => "block not owned by allocator",
        }
    }
}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Result type used throughout the crate.
pub type AllocResult<T> = Result<T, AllocError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_dense_and_stable() {
        for (i, e) in AllocError::ALL.iter().enumerate() {
            assert_eq!(e.code() as usize, i + 1);
            assert!(!e.as_str().is_empty());
        }
        assert_eq!(AllocError::NotOwned.to_string(), "block not owned by allocator");
    }
}
