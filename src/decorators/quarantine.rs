//! Use-after-free detection through quarantine and poisoning.
//!
//! A freed block is filled with [`POISON`] and held back from the wrapped allocator, so it cannot be
//! reused straight away. [`Quarantine::check`] reads every held block; any byte that changed means
//! something wrote to the block after it was freed, and the check reports `Corruption`. Releasing a
//! block that is already held is refused as a double free.
//!
//! The quarantine holds at most `max_blocks` blocks. Older ones are released to the wrapped
//! allocator, so the delay is bounded. Dropping the quarantine flushes it, so nothing leaks.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use core::cell::RefCell;
use std::collections::VecDeque;

/// Byte written over freed memory.
pub const POISON: u8 = 0xDD;

/// Decorator that quarantines freed blocks and checks them for writes after free.
pub struct Quarantine<'a, A: Allocator> {
    inner: &'a A,
    held: RefCell<VecDeque<Block>>,
    max_blocks: usize,
}

impl<'a, A: Allocator> Quarantine<'a, A> {
    /// Wraps `inner`, holding at most `max_blocks` freed blocks.
    pub fn new(inner: &'a A, max_blocks: usize) -> Self {
        Quarantine { inner, held: RefCell::new(VecDeque::new()), max_blocks }
    }

    /// Number of blocks currently quarantined.
    pub fn held(&self) -> usize {
        self.held.borrow().len()
    }

    /// Checks that every quarantined block still holds the poison pattern.
    ///
    /// Returns `Corruption` if any byte has changed since the block was freed.
    pub fn check(&self) -> AllocResult<()> {
        for block in self.held.borrow().iter() {
            // SAFETY: quarantined blocks are still allocated by the wrapped allocator, so the memory is readable.
            let bytes = unsafe { core::slice::from_raw_parts(block.ptr(), block.size()) };
            if bytes.iter().any(|&b| b != POISON) {
                return Err(AllocError::Corruption);
            }
        }
        Ok(())
    }

    /// Releases every quarantined block to the wrapped allocator.
    pub fn flush(&self) -> AllocResult<()> {
        let mut held = self.held.borrow_mut();
        let mut first: Option<AllocError> = None;
        while let Some(block) = held.pop_front() {
            // SAFETY: quarantined blocks are live in the wrapped allocator and are released exactly once, here.
            if let Err(e) = unsafe { self.inner.deallocate(block) } {
                first.get_or_insert(e);
            }
        }
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl<A: Allocator> Allocator for Quarantine<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        self.inner.allocate(size, align)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return Ok(());
        }
        if self.held.borrow().iter().any(|b| b.ptr() == block.ptr()) {
            return Err(AllocError::DoubleFree);
        }
        // SAFETY: the caller guarantees the block is live; its contents are overwritten before it is held.
        unsafe { core::ptr::write_bytes(block.ptr(), POISON, block.size()) };
        let mut held = self.held.borrow_mut();
        held.push_back(block);
        let mut first: Option<AllocError> = None;
        while held.len() > self.max_blocks {
            if let Some(old) = held.pop_front() {
                // SAFETY: the oldest quarantined block is live in the wrapped allocator and leaves quarantine now.
                if let Err(e) = unsafe { self.inner.deallocate(old) } {
                    first.get_or_insert(e);
                }
            }
        }
        match first {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}

impl<A: Allocator> Drop for Quarantine<'_, A> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}
