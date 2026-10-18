//! Error hooks (feature 31). A process-wide callback that runs whenever a `Reporting` decorator sees an
//! allocator error. Use it to log failures or to raise a crash report.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use std::sync::{Mutex, PoisonError};

static HOOK: Mutex<Option<fn(AllocError)>> = Mutex::new(None);

/// Installs `hook` as the process-wide error callback, replacing any previous one.
pub fn set_error_hook(hook: fn(AllocError)) {
    *HOOK.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook);
}

/// Removes the error callback.
pub fn clear_error_hook() {
    *HOOK.lock().unwrap_or_else(PoisonError::into_inner) = None;
}

/// Runs the installed callback, if any, with `error`.
pub fn notify(error: AllocError) {
    let hook = *HOOK.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(hook) = hook {
        hook(error);
    }
}

/// Decorator that reports every error from the wrapped allocator to the error hook.
pub struct Reporting<'a, A: Allocator> {
    inner: &'a A,
}

impl<'a, A: Allocator> Reporting<'a, A> {
    /// Wraps `inner`.
    pub fn new(inner: &'a A) -> Self {
        Reporting { inner }
    }
}

impl<A: Allocator> Allocator for Reporting<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        let result = self.inner.allocate(size, align);
        if let Err(e) = result {
            notify(e);
        }
        result
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        // SAFETY: the caller guarantees the block is live and from the wrapped allocator.
        let result = unsafe { self.inner.deallocate(block) };
        if let Err(e) = result {
            notify(e);
        }
        result
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}
