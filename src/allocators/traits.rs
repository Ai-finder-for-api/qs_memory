//! The `Allocator` trait and an RAII owner for blocks.
//!
//! Contract every allocator satisfies:
//! - `allocate(size, align)`: a zero size succeeds with an empty block. A
//!   non-power-of-two alignment (or one above `MAX_ALIGNMENT`) fails with
//!   `InvalidAlignment`. A successful non-empty block is aligned to `align` and holds
//!   `size` usable bytes.
//! - `deallocate(block)` (unsafe): the block must be exactly what `allocate` returned,
//!   and must not have been released already. An empty block is a no-op. Allocators
//!   check what they can check safely (range, alignment, bookkeeping); callers must
//!   not pass pointers that were never returned by the allocator.
//!
//! Methods take `&self`. Single-threaded allocators keep their bookkeeping in `Cell`s,
//! so the compiler rejects sharing them across threads.

use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::AllocResult;
use core::mem::ManuallyDrop;

/// A source of memory blocks.
pub trait Allocator {
    /// Obtains a block of `size` bytes aligned to `align`.
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block>;

    /// Releases a block returned by this allocator.
    ///
    /// # Safety
    /// `block` must have been returned by this allocator's `allocate`, must still be
    /// live, and must not be used afterwards.
    unsafe fn deallocate(&self, block: Block) -> AllocResult<()>;

    /// What this allocator supports.
    fn capabilities(&self) -> Capabilities;
}

/// Owns one block from an allocator and returns it when dropped.
#[must_use = "dropping an OwnedBlock releases its memory"]
pub struct OwnedBlock<'a, A: Allocator + ?Sized> {
    alloc: &'a A,
    block: Block,
}

impl<'a, A: Allocator + ?Sized> OwnedBlock<'a, A> {
    /// Takes ownership of `block`, which must have come from `alloc`.
    pub fn new(alloc: &'a A, block: Block) -> Self {
        Self { alloc, block }
    }

    /// The owned block.
    pub fn block(&self) -> Block {
        self.block
    }

    /// Releases the block now and reports the allocator's verdict.
    pub fn free(self) -> AllocResult<()> {
        let me = ManuallyDrop::new(self);
        // SAFETY: `me.block` came from `me.alloc` and has not been released; the
        // `ManuallyDrop` ensures `Drop` cannot release it a second time.
        unsafe { me.alloc.deallocate(me.block) }
    }

    /// Gives up ownership without releasing the block.
    pub fn leak(self) -> Block {
        ManuallyDrop::new(self).block
    }
}

impl<A: Allocator + ?Sized> Drop for OwnedBlock<'_, A> {
    fn drop(&mut self) {
        // SAFETY: the owner is the only holder of this block (checked by construction
        // through `new`/`make_owned`), so releasing it here is the single release.
        let _ = unsafe { self.alloc.deallocate(self.block) };
    }
}

/// Allocates and wraps the result in an owner.
pub fn make_owned<'a, A: Allocator + ?Sized>(
    alloc: &'a A,
    size: usize,
    align: usize,
) -> AllocResult<OwnedBlock<'a, A>> {
    let block = alloc.allocate(size, align)?;
    Ok(OwnedBlock::new(alloc, block))
}