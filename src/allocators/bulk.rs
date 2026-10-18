//! Bulk allocation: many blocks in one call. Allocation is all-or-nothing; release continues past errors.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::error::{AllocError, AllocResult};

/// Bulk operations available on every allocator.
pub trait BulkAllocator: Allocator {
    /// Allocates one block of each size in `sizes`, all with alignment `align`.
    ///
    /// If any request fails, every block obtained so far is released and the error is returned,
    /// so the allocator is left as it was before the call.
    fn allocate_many(&self, sizes: &[usize], align: usize) -> AllocResult<Vec<Block>> {
        let mut out: Vec<Block> = Vec::with_capacity(sizes.len());
        for &size in sizes {
            match self.allocate(size, align) {
                Ok(block) => out.push(block),
                Err(e) => {
                    for block in out.drain(..) {
                        // SAFETY: every block in `out` was returned by this allocator a moment ago
                        // and has not been handed to anyone else.
                        let _ = unsafe { self.deallocate(block) };
                    }
                    return Err(e);
                }
            }
        }
        Ok(out)
    }

    /// Releases every block in `blocks` and empties the vector.
    ///
    /// Continues past errors and returns the first one, so one bad block cannot stop the rest from
    /// being released.
    ///
    /// # Safety
    /// Each block must be live, must have come from this allocator, and must appear only once.
    unsafe fn deallocate_many(&self, blocks: &mut Vec<Block>) -> AllocResult<()> {
        let mut first: Option<AllocError> = None;
        for block in blocks.drain(..) {
            // SAFETY: the caller guarantees each block is live, from this allocator, and listed once.
            if let Err(e) = unsafe { self.deallocate(block) } {
                first.get_or_insert(e);
            }
        }
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl<A: Allocator + ?Sized> BulkAllocator for A {}
