//! Resizing a live block: `grow`, `shrink` and `realloc`.
//!
//! Every allocator gets these through a blanket implementation. Each one allocates a new block,
//! copies the bytes, and releases the old block. If releasing the old block fails, the new block is
//! released again and the error is returned, so nothing leaks on the error path.
//!
//! Allocators that can resize in place (for example the free list, when the next block is free)
//! may later override these with faster versions; callers see the same contract.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::error::{AllocError, AllocResult};

/// Resizing operations for any allocator.
pub trait Resize: Allocator {
    /// Moves `block` into a larger block of `new_size` bytes, keeping its contents.
    ///
    /// Fails with `InvalidArgument` if `new_size` is smaller than the current size.
    ///
    /// # Safety
    /// `block` must be live and must have come from `self`. On success the old block must not be
    /// used again.
    unsafe fn grow(&self, block: Block, new_size: usize) -> AllocResult<Block> {
        if new_size < block.size() {
            return Err(AllocError::InvalidArgument);
        }
        let fresh = self.allocate(new_size, block.align())?;
        // SAFETY: both blocks are live and distinct, and `fresh` holds at least `block.size()` bytes.
        unsafe { core::ptr::copy_nonoverlapping(block.ptr(), fresh.ptr(), block.size()) };
        // SAFETY: the caller guarantees `block` is live and from `self`.
        if let Err(e) = unsafe { self.deallocate(block) } {
            // SAFETY: `fresh` was allocated just above and has not been shared with anyone.
            let _ = unsafe { self.deallocate(fresh) };
            return Err(e);
        }
        Ok(fresh)
    }

    /// Moves `block` into a smaller block of `new_size` bytes, keeping the first `new_size` bytes.
    ///
    /// Shrinking to zero releases the block and returns an empty block with the same alignment.
    /// Fails with `InvalidArgument` if `new_size` is larger than the current size.
    ///
    /// # Safety
    /// Same contract as [`Resize::grow`].
    unsafe fn shrink(&self, block: Block, new_size: usize) -> AllocResult<Block> {
        if new_size > block.size() {
            return Err(AllocError::InvalidArgument);
        }
        if new_size == block.size() {
            return Ok(block);
        }
        if new_size == 0 {
            // SAFETY: the caller guarantees `block` is live and from `self`.
            unsafe { self.deallocate(block)? };
            return Ok(Block::empty(block.align()));
        }
        let fresh = self.allocate(new_size, block.align())?;
        // SAFETY: both blocks are live and distinct; `fresh` holds at least `new_size` bytes.
        unsafe { core::ptr::copy_nonoverlapping(block.ptr(), fresh.ptr(), new_size) };
        // SAFETY: the caller guarantees `block` is live and from `self`.
        if let Err(e) = unsafe { self.deallocate(block) } {
            // SAFETY: `fresh` was allocated just above and has not been shared with anyone.
            let _ = unsafe { self.deallocate(fresh) };
            return Err(e);
        }
        Ok(fresh)
    }

    /// Resizes `block` to at least `new_size` bytes.
    ///
    /// - An empty block is allocated fresh.
    /// - `new_size == 0` releases the block.
    /// - A request that fits the current block returns it unchanged. The returned block may keep
    ///   spare capacity, so its `size()` can exceed `new_size`; use [`Resize::shrink`] to release it.
    /// - Otherwise the block is grown.
    ///
    /// # Safety
    /// Same contract as [`Resize::grow`].
    unsafe fn realloc(&self, block: Block, new_size: usize) -> AllocResult<Block> {
        if block.is_empty() {
            return self.allocate(new_size, block.align());
        }
        if new_size == 0 {
            // SAFETY: the caller guarantees `block` is live and from `self`.
            unsafe { self.deallocate(block)? };
            return Ok(Block::empty(block.align()));
        }
        if new_size <= block.size() {
            return Ok(block);
        }
        // SAFETY: the caller guarantees `block` is live and from `self`.
        unsafe { self.grow(block, new_size) }
    }
}

impl<A: Allocator + ?Sized> Resize for A {}
