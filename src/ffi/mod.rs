//! C ABI for qs_crypt and other C callers. Declared in `include/qs_memory.h`.
//!
//! Conventions:
//! - Every fallible function returns a status byte: `0` on success, otherwise the
//!   `AllocError` code (see `qs_error_str`).
//! - Handles are opaque pointers created by the `*_create` functions and released by the
//!   matching `*_destroy` function.
//! - Pointers handed out by `qs_alloc` and the handle allocators are released with the
//!   matching free function. Passing anything else is undefined behaviour.
//!
//! Each function checks its pointer arguments for null before use.

use core::mem::{align_of, size_of};
use core::ptr;

use crate::platform;
use crate::{AllocError, Allocator, Arena, Block, FreeListAllocator, PoolAllocator, SystemAllocator};

const OK: u8 = 0;

static SYSTEM: SystemAllocator = SystemAllocator::new();

fn invalid() -> u8 {
    AllocError::InvalidArgument.code()
}

/// Moves `value` into raw storage owned by the caller of `unbox`.
fn box_raw<T>(value: T) -> Result<*mut T, AllocError> {
    let p = platform::raw_allocate(size_of::<T>(), align_of::<T>()) as *mut T;
    if p.is_null() {
        return Err(AllocError::OutOfMemory);
    }
    // SAFETY: `p` is a fresh allocation of `size_of::<T>()` bytes with the right alignment.
    unsafe { ptr::write(p, value) };
    Ok(p)
}

/// Drops and releases a value created by `box_raw`.
///
/// # Safety
/// `p` must come from `box_raw::<T>` and must not be used afterwards.
unsafe fn unbox<T>(p: *mut T) {
    // SAFETY: per the contract, `p` holds a live value created by `box_raw`.
    unsafe {
        ptr::drop_in_place(p);
        platform::raw_deallocate(p as *mut u8);
    }
}

const MESSAGES: [&[u8]; 14] = [
    b"none\0",
    b"invalid argument\0",
    b"invalid alignment\0",
    b"size overflow\0",
    b"out of memory\0",
    b"capacity exhausted\0",
    b"invalid free\0",
    b"double free\0",
    b"invalid checkpoint\0",
    b"stale checkpoint\0",
    b"out-of-order free\0",
    b"not supported\0",
    b"memory corruption detected\0",
    b"block not owned by allocator\0",
];

/// Library version as a NUL-terminated string. Never freed.
#[no_mangle]
pub extern "C" fn qs_memory_version() -> *const u8 {
    b"0.1.0\0".as_ptr()
}

/// Message for a status code, NUL-terminated. Unknown codes map to "none". Never freed.
#[no_mangle]
pub extern "C" fn qs_error_str(code: u8) -> *const u8 {
    MESSAGES.get(code as usize).copied().unwrap_or(MESSAGES[0]).as_ptr()
}

/// Allocates `size` bytes aligned to `align` from the system heap.
///
/// # Safety
/// `out` must be a valid place to write a pointer.
#[no_mangle]
pub unsafe extern "C" fn qs_alloc(size: usize, align: usize, out: *mut *mut u8) -> u8 {
    if out.is_null() {
        return invalid();
    }
    match SYSTEM.allocate(size, align) {
        Ok(b) => {
            // SAFETY: `out` is non-null and writable per the contract.
            unsafe { *out = b.ptr() };
            OK
        }
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

/// Releases memory from `qs_alloc`.
///
/// # Safety
/// `p`, `size` and `align` must describe a block returned by `qs_alloc` and not yet released.
#[no_mangle]
pub unsafe extern "C" fn qs_free(p: *mut u8, size: usize, align: usize) -> u8 {
    if p.is_null() {
        return if size == 0 { OK } else { invalid() };
    }
    // SAFETY: per the contract, the block came from `qs_alloc` and is live.
    match unsafe { SYSTEM.deallocate(Block::from_raw_parts(p, size, align)) } {
        Ok(()) => OK,
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

// ----------------------------------------------------------------- arenas

/// Creates an arena owning `capacity` bytes.
///
/// # Safety
/// `out` must be a valid place to write a handle.
#[no_mangle]
pub unsafe extern "C" fn qs_arena_create(capacity: usize, out: *mut *mut Arena<'static>) -> u8 {
    if out.is_null() {
        return invalid();
    }
    let arena = match Arena::new(capacity) {
        Ok(a) => a,
        Err(e) => return crate::ffi::heaps::ffi_code(e),
    };
    match box_raw(arena) {
        Ok(p) => {
            // SAFETY: `out` is non-null and writable per the contract.
            unsafe { *out = p };
            OK
        }
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

/// Allocates from an arena.
///
/// # Safety
/// `arena` must be a live handle from `qs_arena_create`; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_arena_alloc(arena: *mut Arena<'static>, size: usize, align: usize, out: *mut *mut u8) -> u8 {
    if arena.is_null() || out.is_null() {
        return invalid();
    }
    // SAFETY: `arena` is a live handle per the contract.
    match unsafe { (*arena).allocate(size, align) } {
        Ok(b) => {
            // SAFETY: `out` is writable per the contract.
            unsafe { *out = b.ptr() };
            OK
        }
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

/// Releases everything an arena handed out.
///
/// # Safety
/// `arena` must be a live handle from `qs_arena_create`.
#[no_mangle]
pub unsafe extern "C" fn qs_arena_reset(arena: *mut Arena<'static>) -> u8 {
    if arena.is_null() {
        return invalid();
    }
    // SAFETY: `arena` is live per the contract.
    unsafe { (*arena).reset() };
    OK
}

/// Destroys an arena and its storage.
///
/// # Safety
/// `arena` must be a live handle from `qs_arena_create`; it must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn qs_arena_destroy(arena: *mut Arena<'static>) -> u8 {
    if arena.is_null() {
        return invalid();
    }
    // SAFETY: per the contract, `arena` came from `box_raw` and is released once here.
    unsafe { unbox(arena) };
    OK
}

// ------------------------------------------------------------------ pools

/// Creates a fixed-size pool.
///
/// # Safety
/// `out` must be a valid place to write a handle.
#[no_mangle]
pub unsafe extern "C" fn qs_pool_create(object_size: usize, object_align: usize, count: usize, out: *mut *mut PoolAllocator) -> u8 {
    if out.is_null() {
        return invalid();
    }
    let pool = match PoolAllocator::create(object_size, object_align, count) {
        Ok(p) => p,
        Err(e) => return crate::ffi::heaps::ffi_code(e),
    };
    match box_raw(pool) {
        Ok(p) => {
            // SAFETY: `out` is writable per the contract.
            unsafe { *out = p };
            OK
        }
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

/// Takes one object from a pool.
///
/// # Safety
/// `pool` must be a live handle from `qs_pool_create`; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_pool_alloc(pool: *mut PoolAllocator, size: usize, align: usize, out: *mut *mut u8) -> u8 {
    if pool.is_null() || out.is_null() {
        return invalid();
    }
    // SAFETY: `pool` is live per the contract.
    match unsafe { (*pool).allocate(size, align) } {
        Ok(b) => {
            // SAFETY: `out` is writable per the contract.
            unsafe { *out = b.ptr() };
            OK
        }
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

/// Returns one object to a pool.
///
/// # Safety
/// `pool` must be a live handle; `p` must be an object from `qs_pool_alloc` on it.
#[no_mangle]
pub unsafe extern "C" fn qs_pool_free(pool: *mut PoolAllocator, p: *mut u8, size: usize, align: usize) -> u8 {
    if pool.is_null() {
        return invalid();
    }
    // SAFETY: `pool` is live per the contract.
    match unsafe { (*pool).deallocate(Block::from_raw_parts(p, size, align)) } {
        Ok(()) => OK,
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

/// Destroys a pool and its storage.
///
/// # Safety
/// `pool` must be a live handle from `qs_pool_create`; it must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn qs_pool_destroy(pool: *mut PoolAllocator) -> u8 {
    if pool.is_null() {
        return invalid();
    }
    // SAFETY: per the contract, `pool` came from `box_raw` and is released once here.
    unsafe { unbox(pool) };
    OK
}

// -------------------------------------------------------------- free lists

/// Creates a coalescing free-list heap.
///
/// # Safety
/// `out` must be a valid place to write a handle.
#[no_mangle]
pub unsafe extern "C" fn qs_freelist_create(capacity: usize, out: *mut *mut FreeListAllocator) -> u8 {
    if out.is_null() {
        return invalid();
    }
    let heap = match FreeListAllocator::create(capacity) {
        Ok(h) => h,
        Err(e) => return crate::ffi::heaps::ffi_code(e),
    };
    match box_raw(heap) {
        Ok(p) => {
            // SAFETY: `out` is writable per the contract.
            unsafe { *out = p };
            OK
        }
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

/// Allocates from a free-list heap.
///
/// # Safety
/// `heap` must be a live handle; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_freelist_alloc(heap: *mut FreeListAllocator, size: usize, align: usize, out: *mut *mut u8) -> u8 {
    if heap.is_null() || out.is_null() {
        return invalid();
    }
    // SAFETY: `heap` is live per the contract.
    match unsafe { (*heap).allocate(size, align) } {
        Ok(b) => {
            // SAFETY: `out` is writable per the contract.
            unsafe { *out = b.ptr() };
            OK
        }
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

/// Releases a block from a free-list heap.
///
/// # Safety
/// `heap` must be a live handle; `p`, `size` and `align` must describe a live block from it.
#[no_mangle]
pub unsafe extern "C" fn qs_freelist_free(heap: *mut FreeListAllocator, p: *mut u8, size: usize, align: usize) -> u8 {
    if heap.is_null() {
        return invalid();
    }
    // SAFETY: `heap` is live per the contract.
    match unsafe { (*heap).deallocate(Block::from_raw_parts(p, size, align)) } {
        Ok(()) => OK,
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

/// Checks every invariant of a free-list heap.
///
/// # Safety
/// `heap` must be a live handle.
#[no_mangle]
pub unsafe extern "C" fn qs_freelist_verify(heap: *const FreeListAllocator) -> u8 {
    if heap.is_null() {
        return invalid();
    }
    // SAFETY: `heap` is live per the contract.
    match unsafe { (*heap).verify() } {
        Ok(()) => OK,
        Err(e) => crate::ffi::heaps::ffi_code(e),
    }
}

/// Destroys a free-list heap and its storage.
///
/// # Safety
/// `heap` must be a live handle from `qs_freelist_create`; it must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn qs_freelist_destroy(heap: *mut FreeListAllocator) -> u8 {
    if heap.is_null() {
        return invalid();
    }
    // SAFETY: per the contract, `heap` came from `box_raw` and is released once here.
    unsafe { unbox(heap) };
    OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;

    #[test]
    fn messages_are_nul_terminated_and_cover_codes() {
        for code in 0..=13u8 {
            let p = qs_error_str(code);
            // SAFETY: the table entries are static NUL-terminated strings.
            let s = unsafe { CStr::from_ptr(p as *const std::ffi::c_char) };
            assert!(!s.to_bytes().is_empty());
        }
        assert_eq!(unsafe { CStr::from_ptr(qs_error_str(200) as *const std::ffi::c_char) }.to_str().unwrap(), "none");
    }

    #[test]
    fn arena_handle_round_trip() {
        let mut h: *mut Arena<'static> = ptr::null_mut();
        let mut p: *mut u8 = ptr::null_mut();
        // SAFETY: every call receives valid out-pointers and a handle it created.
        unsafe {
            assert_eq!(qs_arena_create(256, &mut h), 0);
            assert_eq!(qs_arena_alloc(h, 32, 16, &mut p), 0);
            assert!(!p.is_null());
            assert_eq!(qs_arena_alloc(h, 1024, 1, &mut p), AllocError::CapacityExhausted.code());
            assert_eq!(qs_arena_reset(h), 0);
            assert_eq!(qs_arena_destroy(h), 0);
        }
    }

    #[test]
    fn pool_and_freelist_round_trip_with_misuse() {
        let mut pool: *mut PoolAllocator = ptr::null_mut();
        let mut heap: *mut FreeListAllocator = ptr::null_mut();
        let mut p: *mut u8 = ptr::null_mut();
        // SAFETY: handles and out-pointers are valid for each call; misuse is refused.
        unsafe {
            assert_eq!(qs_pool_create(32, 16, 2, &mut pool), 0);
            assert_eq!(qs_pool_alloc(pool, 32, 16, &mut p), 0);
            assert_eq!(qs_pool_free(pool, p, 32, 16), 0);
            assert_eq!(qs_pool_free(pool, p, 32, 16), AllocError::DoubleFree.code());
            assert_eq!(qs_pool_destroy(pool), 0);

            assert_eq!(qs_freelist_create(4096, &mut heap), 0);
            assert_eq!(qs_freelist_alloc(heap, 100, 16, &mut p), 0);
            assert_eq!(qs_freelist_verify(heap), 0);
            assert_eq!(qs_freelist_free(heap, p, 100, 16), 0);
            assert_eq!(qs_freelist_verify(heap), 0);
            assert_eq!(qs_freelist_destroy(heap), 0);
        }
    }

    #[test]
    fn null_arguments_are_refused() {
        let mut p: *mut u8 = ptr::null_mut();
        // SAFETY: deliberately null handles; each call refuses them before dereferencing.
        unsafe {
            assert_eq!(qs_alloc(16, 16, ptr::null_mut()), AllocError::InvalidArgument.code());
            assert_eq!(qs_arena_alloc(ptr::null_mut(), 8, 8, &mut p), AllocError::InvalidArgument.code());
            assert_eq!(qs_pool_destroy(ptr::null_mut()), AllocError::InvalidArgument.code());
            assert_eq!(qs_alloc(16, 16, &mut p), 0);
            assert_eq!(qs_free(p, 16, 16), 0);
        }
    }

    #[test]
    fn align_import_is_used() {
        // Keeps the crate-level alignment constant reachable from the ABI tests.
        assert!(crate::config::DEFAULT_ALIGNMENT.is_power_of_two());
    }
}
pub mod heaps;
pub mod extras;
