//! Introspection: allocators that can confirm a block is a live allocation without touching its memory.
//! Decorators that write to a block (such as zeroing) require this, so they never act on memory the
//! wrapped allocator does not own.

use crate::foundation::block::Block;

/// Confirms that a block is a live allocation of this allocator.
pub trait LiveBlocks {
    /// True when `block` is exactly a block this allocator handed out and has not yet released, with
    /// a size no larger than the allocation. Must not read the block's memory.
    fn is_live(&self, block: Block) -> bool;
}
