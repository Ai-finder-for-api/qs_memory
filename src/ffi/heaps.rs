//! C ABI for the additional allocators: buddy, TLSF, ring, slab and stack.
//!
//! Each handle is created by a `*_create` function and released by the matching `*_destroy`. Every
//! function returns a status code: 0 is success, anything else is an `AllocError` code. Pointers returned
//! by `*_alloc` are released with `*_free`, passing the same size and alignment.

use crate::{AllocError, AllocResult, Allocator, Block, BuddyAllocator, RingAllocator, SlabAllocator, StackAllocator, Tlsf};

const OK: u8 = 0;

fn code(e: AllocError) -> u8 {
    crate::notify(e);
    e as u8
}

unsafe fn create_in<A>(value: AllocResult<A>, out: *mut *mut A) -> u8 {
    if out.is_null() {
        return code(AllocError::InvalidArgument);
    }
    match value {
        Ok(heap) => {
            // SAFETY: `out` is non-null and writable by the caller's contract.
            unsafe { *out = Box::into_raw(Box::new(heap)) };
            OK
        }
        Err(e) => code(e),
    }
}

unsafe fn alloc_in<A: Allocator>(h: *mut A, size: usize, align: usize, out: *mut *mut u8) -> u8 {
    if h.is_null() || out.is_null() {
        return code(AllocError::InvalidArgument);
    }
    // SAFETY: `h` is a live handle created by the matching create function.
    match unsafe { (*h).allocate(size, align) } {
        Ok(block) => {
            // SAFETY: `out` is non-null and writable by the caller's contract.
            unsafe { *out = block.ptr() };
            OK
        }
        Err(e) => code(e),
    }
}

unsafe fn free_in<A: Allocator>(h: *mut A, p: *mut u8, size: usize, align: usize) -> u8 {
    if h.is_null() {
        return code(AllocError::InvalidArgument);
    }
    // SAFETY: the caller passes a pointer returned by `*_alloc` with the same size and alignment.
    let block = unsafe { Block::from_raw_parts(p, size, align) };
    // SAFETY: `h` is a live handle; the allocator checks the block before acting on it.
    match unsafe { (*h).deallocate(block) } {
        Ok(()) => OK,
        Err(e) => code(e),
    }
}

unsafe fn destroy_in<A>(h: *mut A) {
    if !h.is_null() {
        // SAFETY: `h` came from `Box::into_raw` in the create function and is released once, here.
        drop(unsafe { Box::from_raw(h) });
    }
}

/// Creates a buddy heap of `capacity` bytes (rounded to a power of two).
///
/// # Safety
/// `out` must be a valid pointer to writable storage for one handle.
#[no_mangle]
pub unsafe extern "C" fn qs_buddy_create(capacity: usize, out: *mut *mut BuddyAllocator) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { create_in(BuddyAllocator::create(capacity), out) }
}

/// # Safety
/// `h` must be a live buddy handle; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_buddy_alloc(h: *mut BuddyAllocator, size: usize, align: usize, out: *mut *mut u8) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { alloc_in(h, size, align, out) }
}

/// # Safety
/// `h` must be a live buddy handle; `p` must come from `qs_buddy_alloc` on `h`.
#[no_mangle]
pub unsafe extern "C" fn qs_buddy_free(h: *mut BuddyAllocator, p: *mut u8, size: usize, align: usize) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { free_in(h, p, size, align) }
}

/// # Safety
/// `h` must be a live buddy handle.
#[no_mangle]
pub unsafe extern "C" fn qs_buddy_verify(h: *const BuddyAllocator) -> u8 {
    if h.is_null() {
        return code(AllocError::InvalidArgument);
    }
    // SAFETY: `h` is live by the caller's contract.
    match unsafe { (*h).verify() } {
        Ok(()) => OK,
        Err(e) => code(e),
    }
}

/// # Safety
/// `h` must be a handle from `qs_buddy_create`, destroyed at most once.
#[no_mangle]
pub unsafe extern "C" fn qs_buddy_destroy(h: *mut BuddyAllocator) {
    // SAFETY: forwarded from this function's contract.
    unsafe { destroy_in(h) }
}

/// Creates a TLSF heap of `capacity` bytes.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_tlsf_create(capacity: usize, out: *mut *mut Tlsf) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { create_in(Tlsf::new(capacity), out) }
}

/// # Safety
/// `h` must be a live TLSF handle; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_tlsf_alloc(h: *mut Tlsf, size: usize, align: usize, out: *mut *mut u8) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { alloc_in(h, size, align, out) }
}

/// # Safety
/// `h` must be a live TLSF handle; `p` must come from `qs_tlsf_alloc` on `h`.
#[no_mangle]
pub unsafe extern "C" fn qs_tlsf_free(h: *mut Tlsf, p: *mut u8, size: usize, align: usize) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { free_in(h, p, size, align) }
}

/// Usage counters, mirroring `qs_tlsf_usage` in the header.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct QsTlsfUsage {
    pub free_blocks: usize,
    pub used_blocks: usize,
    pub free_bytes: usize,
    pub used_bytes: usize,
    pub largest_free: usize,
}

/// # Safety
/// `h` must be a live TLSF handle; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_tlsf_usage(h: *const Tlsf, out: *mut QsTlsfUsage) -> u8 {
    if h.is_null() || out.is_null() {
        return code(AllocError::InvalidArgument);
    }
    // SAFETY: `h` is live by the caller's contract.
    let u = unsafe { (*h).usage() };
    // SAFETY: `out` is writable by the caller's contract.
    unsafe {
        *out = QsTlsfUsage {
            free_blocks: u.free_blocks,
            used_blocks: u.used_blocks,
            free_bytes: u.free_bytes,
            used_bytes: u.used_bytes,
            largest_free: u.largest_free,
        }
    };
    OK
}

/// # Safety
/// `h` must be a live TLSF handle.
#[no_mangle]
pub unsafe extern "C" fn qs_tlsf_verify(h: *const Tlsf) -> u8 {
    if h.is_null() {
        return code(AllocError::InvalidArgument);
    }
    // SAFETY: `h` is live by the caller's contract.
    match unsafe { (*h).verify() } {
        Ok(()) => OK,
        Err(e) => code(e),
    }
}

/// # Safety
/// `h` must be a handle from `qs_tlsf_create`, destroyed at most once.
#[no_mangle]
pub unsafe extern "C" fn qs_tlsf_destroy(h: *mut Tlsf) {
    // SAFETY: forwarded from this function's contract.
    unsafe { destroy_in(h) }
}

/// Creates a ring buffer of `capacity` bytes.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_ring_create(capacity: usize, out: *mut *mut RingAllocator) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { create_in(RingAllocator::new(capacity), out) }
}

/// # Safety
/// `h` must be a live ring handle; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_ring_alloc(h: *mut RingAllocator, size: usize, align: usize, out: *mut *mut u8) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { alloc_in(h, size, align, out) }
}

/// Releases the oldest block. Releasing out of order returns `OutOfOrderFree`.
///
/// # Safety
/// `h` must be a live ring handle; `p` must come from `qs_ring_alloc` on `h`.
#[no_mangle]
pub unsafe extern "C" fn qs_ring_free(h: *mut RingAllocator, p: *mut u8, size: usize, align: usize) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { free_in(h, p, size, align) }
}

/// # Safety
/// `h` must be a handle from `qs_ring_create`, destroyed at most once.
#[no_mangle]
pub unsafe extern "C" fn qs_ring_destroy(h: *mut RingAllocator) {
    // SAFETY: forwarded from this function's contract.
    unsafe { destroy_in(h) }
}

/// Creates a slab cache for objects of `object_size` bytes aligned to `object_align`.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_slab_create(object_size: usize, object_align: usize, out: *mut *mut SlabAllocator) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { create_in(SlabAllocator::create(object_size, object_align), out) }
}

/// # Safety
/// `h` must be a live slab handle; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_slab_alloc(h: *mut SlabAllocator, size: usize, align: usize, out: *mut *mut u8) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { alloc_in(h, size, align, out) }
}

/// # Safety
/// `h` must be a live slab handle; `p` must come from `qs_slab_alloc` on `h`.
#[no_mangle]
pub unsafe extern "C" fn qs_slab_free(h: *mut SlabAllocator, p: *mut u8, size: usize, align: usize) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { free_in(h, p, size, align) }
}

/// # Safety
/// `h` must be a handle from `qs_slab_create`, destroyed at most once.
#[no_mangle]
pub unsafe extern "C" fn qs_slab_destroy(h: *mut SlabAllocator) {
    // SAFETY: forwarded from this function's contract.
    unsafe { destroy_in(h) }
}

/// Creates a LIFO stack of `capacity` bytes.
///
/// # Safety
/// `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_stack_create(capacity: usize, out: *mut *mut StackAllocator) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { create_in(StackAllocator::new(capacity), out) }
}

/// # Safety
/// `h` must be a live stack handle; `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn qs_stack_alloc(h: *mut StackAllocator, size: usize, align: usize, out: *mut *mut u8) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { alloc_in(h, size, align, out) }
}

/// Releases the newest block. Releasing out of LIFO order returns `OutOfOrderFree`.
///
/// # Safety
/// `h` must be a live stack handle; `p` must come from `qs_stack_alloc` on `h`.
#[no_mangle]
pub unsafe extern "C" fn qs_stack_free(h: *mut StackAllocator, p: *mut u8, size: usize, align: usize) -> u8 {
    // SAFETY: forwarded from this function's contract.
    unsafe { free_in(h, p, size, align) }
}

/// # Safety
/// `h` must be a handle from `qs_stack_create`, destroyed at most once.
#[no_mangle]
pub unsafe extern "C" fn qs_stack_destroy(h: *mut StackAllocator) {
    // SAFETY: forwarded from this function's contract.
    unsafe { destroy_in(h) }
}

/// Error code for the older C functions, which also report the error through the hook.
pub(crate) fn ffi_code(e: AllocError) -> u8 {
    code(e)
}

/// Turns zero-on-free on or off for a TLSF heap.
///
/// # Safety
/// `h` must be a live TLSF handle.
#[no_mangle]
pub unsafe extern "C" fn qs_tlsf_set_zero_on_free(h: *const Tlsf, on: u8) -> u8 {
    if h.is_null() {
        return code(AllocError::InvalidArgument);
    }
    // SAFETY: `h` is live by the caller's contract.
    unsafe { (*h).set_zero_on_free(on != 0) };
    OK
}
