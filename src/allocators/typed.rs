//! Typed allocation helpers. Size and alignment come from the Rust type, so callers cannot pass the wrong pair.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::error::{AllocError, AllocResult};
use core::mem::{align_of, size_of, size_of_val};

/// Typed helpers available on every allocator.
pub trait AllocatorExt: Allocator {
    /// Allocates raw storage for one `T`, with `T`'s size and alignment.
    fn allocate_for<T>(&self) -> AllocResult<Block> {
        self.allocate(size_of::<T>(), align_of::<T>())
    }

    /// Allocates raw storage for `count` values of `T`.
    ///
    /// Fails with `Overflow` when `count * size_of::<T>()` does not fit in `usize`.
    fn allocate_array<T>(&self, count: usize) -> AllocResult<Block> {
        let bytes = size_of::<T>().checked_mul(count).ok_or(AllocError::Overflow)?;
        self.allocate(bytes, align_of::<T>())
    }

    /// Allocates storage for a copy of `src`, and copies the values in.
    ///
    /// The bytes are copied as plain data, which is why `T: Copy` is required.
    fn allocate_copy_of<T: Copy>(&self, src: &[T]) -> AllocResult<Block> {
        let block = self.allocate_array::<T>(src.len())?;
        // SAFETY: `block` holds `size_of_val(src)` bytes, and `T: Copy` means the bytes are plain data.
        unsafe { core::ptr::copy_nonoverlapping(src.as_ptr() as *const u8, block.ptr(), size_of_val(src)) };
        Ok(block)
    }
}

impl<A: Allocator + ?Sized> AllocatorExt for A {}
