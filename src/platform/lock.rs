//! Page locking (feature 39): keeps pages resident in RAM so secrets are not written to the page file.
//!
//! Locking is page-granular. Unlocking a range also unlocks any neighbouring allocation that shares one
//! of its pages, so callers that keep secrets should allocate them on their own pages.

use crate::platform;

#[cfg(windows)]
mod imp {
    use core::ffi::c_void;

    extern "system" {
        fn VirtualLock(address: *mut c_void, size: usize) -> i32;
        fn VirtualUnlock(address: *mut c_void, size: usize) -> i32;
    }

    pub unsafe fn lock(address: *mut u8, len: usize) -> bool {
        // SAFETY: the caller passes a page-aligned range that it owns.
        unsafe { VirtualLock(address as *mut c_void, len) != 0 }
    }

    pub unsafe fn unlock(address: *mut u8, len: usize) -> bool {
        // SAFETY: the caller passes a page-aligned range that it locked earlier.
        unsafe { VirtualUnlock(address as *mut c_void, len) != 0 }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use core::ffi::{c_int, c_void};

    extern "C" {
        fn mlock(address: *const c_void, len: usize) -> c_int;
        fn munlock(address: *const c_void, len: usize) -> c_int;
    }

    pub unsafe fn lock(address: *mut u8, len: usize) -> bool {
        // SAFETY: the caller passes a page-aligned range that it owns.
        unsafe { mlock(address as *const c_void, len) == 0 }
    }

    pub unsafe fn unlock(address: *mut u8, len: usize) -> bool {
        // SAFETY: the caller passes a page-aligned range that it locked earlier.
        unsafe { munlock(address as *const c_void, len) == 0 }
    }
}

/// The whole pages covering `[p, p + n)`, as `(start, length)`.
fn page_span(p: *mut u8, n: usize) -> (*mut u8, usize) {
    let page = platform::page_size();
    let start = (p as usize) & !(page - 1);
    let end = ((p as usize) + n + page - 1) & !(page - 1);
    (start as *mut u8, end - start)
}

/// Locks every page covering `[p, p + n)`. Returns false when the OS refuses, for example because the
/// process working-set limit is reached. Nothing is locked in that case.
///
/// # Safety
/// `[p, p + n)` must be memory this process owns.
pub unsafe fn lock_pages(p: *mut u8, n: usize) -> bool {
    let (start, len) = page_span(p, n);
    // SAFETY: the span covers whole pages of memory the caller owns.
    unsafe { imp::lock(start, len) }
}

/// Unlocks every page covering `[p, p + n)`.
///
/// # Safety
/// `[p, p + n)` must have been locked with [`lock_pages`].
pub unsafe fn unlock_pages(p: *mut u8, n: usize) -> bool {
    let (start, len) = page_span(p, n);
    // SAFETY: the span covers whole pages that were locked by `lock_pages`.
    unsafe { imp::unlock(start, len) }
}
