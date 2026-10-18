//! C ABI extras: an error callback, system memory queries, a growable byte buffer on the system heap,
//! and a feature mask so C callers can check what the library was built with.

use crate::{AllocError, Allocator, Block, Resize, SystemAllocator};
use std::sync::{Mutex, PoisonError};

const OK: u8 = 0;
const NOT_SUPPORTED: u8 = 11;

static C_HOOK: Mutex<Option<extern "C" fn(u8)>> = Mutex::new(None);

fn c_trampoline(error: AllocError) {
    let callback = *C_HOOK.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(callback) = callback {
        callback(error as u8);
    }
}

/// Installs `callback` as the error callback, or removes it when `callback` is NULL. The callback receives
/// the `AllocError` code of every error reported through the `Reporting` decorator.
#[no_mangle]
pub extern "C" fn qs_set_error_hook(callback: Option<extern "C" fn(u8)>) -> u8 {
    *C_HOOK.lock().unwrap_or_else(PoisonError::into_inner) = callback;
    match callback {
        Some(_) => crate::set_error_hook(c_trampoline),
        None => crate::clear_error_hook(),
    }
    OK
}

/// System memory figures, mirroring `qs_memory_info` in the header.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct QsMemoryInfo {
    pub total_physical: u64,
    pub available_physical: u64,
    pub page_size: usize,
    pub large_page_size: usize,
}

/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_system_memory(out: *mut QsMemoryInfo) -> u8 {
    if out.is_null() {
        return AllocError::InvalidArgument as u8;
    }
    match crate::system_memory() {
        Some(info) => {
            // SAFETY: `out` is writable by the caller's contract.
            unsafe {
                *out = QsMemoryInfo {
                    total_physical: info.total_physical,
                    available_physical: info.available_physical,
                    page_size: info.page_size,
                    large_page_size: info.large_page_size,
                }
            };
            OK
        }
        None => NOT_SUPPORTED,
    }
}

/// A growable byte buffer whose storage comes from the system heap through the library's own `Resize`.
pub struct CBytes {
    sys: SystemAllocator,
    data: Block,
    len: usize,
}

/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_bytes_create(out: *mut *mut CBytes) -> u8 {
    if out.is_null() {
        return AllocError::InvalidArgument as u8;
    }
    let bytes = Box::new(CBytes { sys: SystemAllocator::new(), data: Block::empty(1), len: 0 });
    // SAFETY: `out` is writable by the caller's contract.
    unsafe { *out = Box::into_raw(bytes) };
    OK
}

/// Appends one byte, growing the buffer when it is full.
///
/// # Safety
/// `h` must be a live handle from `qs_bytes_create`.
#[no_mangle]
pub unsafe extern "C" fn qs_bytes_push(h: *mut CBytes, byte: u8) -> u8 {
    if h.is_null() {
        return AllocError::InvalidArgument as u8;
    }
    // SAFETY: `h` is live by the caller's contract and not aliased during this call.
    let bytes = unsafe { &mut *h };
    if bytes.len == bytes.data.size() {
        let wanted = (bytes.data.size() * 2).max(16);
        // SAFETY: `data` is the live block owned by this buffer; the old block is replaced on success.
        match unsafe { bytes.sys.realloc(bytes.data, wanted) } {
            Ok(block) => bytes.data = block,
            Err(e) => return e as u8,
        }
    }
    // SAFETY: `len < data.size()` after the growth above, so the byte is inside the block.
    unsafe { *bytes.data.ptr().add(bytes.len) = byte };
    bytes.len += 1;
    OK
}

/// # Safety
/// `h` must be a live handle or NULL.
#[no_mangle]
pub unsafe extern "C" fn qs_bytes_len(h: *const CBytes) -> usize {
    if h.is_null() {
        0
    } else {
        // SAFETY: `h` is live by the caller's contract.
        unsafe { (*h).len }
    }
}

/// # Safety
/// `h` must be a live handle or NULL. The pointer is valid until the next push or destroy.
#[no_mangle]
pub unsafe extern "C" fn qs_bytes_data(h: *const CBytes) -> *const u8 {
    if h.is_null() {
        core::ptr::null()
    } else {
        // SAFETY: `h` is live by the caller's contract.
        unsafe { (*h).data.ptr() as *const u8 }
    }
}

/// # Safety
/// `h` must be a handle from `qs_bytes_create`, destroyed at most once.
#[no_mangle]
pub unsafe extern "C" fn qs_bytes_destroy(h: *mut CBytes) {
    if h.is_null() {
        return;
    }
    // SAFETY: `h` came from `Box::into_raw` and is released once, here.
    let bytes = unsafe { Box::from_raw(h) };
    let data = bytes.data;
    // SAFETY: `data` is the live block owned by this buffer, released once.
    let _ = unsafe { bytes.sys.deallocate(data) };
    drop(bytes);
}

/// Bit flags describing what this build provides. See `qs_feature` in the header.
pub const FEATURE_TLSF: u32 = 1 << 0;
pub const FEATURE_RING: u32 = 1 << 1;
pub const FEATURE_BUDDY: u32 = 1 << 2;
pub const FEATURE_SLAB: u32 = 1 << 3;
pub const FEATURE_STACK: u32 = 1 << 4;
pub const FEATURE_SHARED_MEMORY: u32 = 1 << 5;
pub const FEATURE_LARGE_PAGES: u32 = 1 << 6;
pub const FEATURE_PAGE_LOCK: u32 = 1 << 7;
pub const FEATURE_THREAD_CACHE: u32 = 1 << 8;
pub const FEATURE_SHARDED: u32 = 1 << 9;
pub const FEATURE_ZEROING: u32 = 1 << 10;
pub const FEATURE_ERROR_HOOK: u32 = 1 << 11;
pub const FEATURE_BYTES: u32 = 1 << 12;
pub const FEATURE_SYSTEM_MEMORY: u32 = 1 << 13;

/// The features this build provides.
#[no_mangle]
pub extern "C" fn qs_features() -> u32 {
    FEATURE_TLSF
        | FEATURE_RING
        | FEATURE_BUDDY
        | FEATURE_SLAB
        | FEATURE_STACK
        | FEATURE_SHARED_MEMORY
        | FEATURE_LARGE_PAGES
        | FEATURE_PAGE_LOCK
        | FEATURE_THREAD_CACHE
        | FEATURE_SHARDED
        | FEATURE_ZEROING
        | FEATURE_ERROR_HOOK
        | FEATURE_BYTES
        | FEATURE_SYSTEM_MEMORY
}
