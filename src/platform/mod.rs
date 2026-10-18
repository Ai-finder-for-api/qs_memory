//! Operating-system interface, declared directly so the crate needs no dependencies.
//!
//! Only the C runtime's aligned allocator and the platform's virtual-memory calls are
//! used. Linux and Windows are supported; other targets do not compile.

#[cfg(not(any(target_os = "linux", windows)))]
compile_error!("qs_memory supports Linux and Windows only");

#[cfg(target_os = "linux")]
mod imp {
    use core::ffi::{c_int, c_long, c_void};
    use core::mem::size_of;

    const PROT_NONE: c_int = 0;
    const PROT_READ: c_int = 1;
    const PROT_WRITE: c_int = 2;
    const MAP_PRIVATE: c_int = 0x02;
    const MAP_ANONYMOUS: c_int = 0x20;
    const MAP_NORESERVE: c_int = 0x4000;
    const MADV_DONTNEED: c_int = 4;
    const SC_PAGESIZE: c_int = 30;

    extern "C" {
        fn posix_memalign(memptr: *mut *mut c_void, alignment: usize, size: usize) -> c_int;
        fn free(ptr: *mut c_void);
        fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
        fn munmap(addr: *mut c_void, len: usize) -> c_int;
        fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;
        fn madvise(addr: *mut c_void, len: usize, advice: c_int) -> c_int;
        fn sysconf(name: c_int) -> c_long;
    }

    /// Aligned storage from the C runtime; null on failure or for `size == 0`.
    pub(crate) fn raw_allocate(size: usize, align: usize) -> *mut u8 {
        if size == 0 {
            return core::ptr::null_mut();
        }
        // posix_memalign requires a power of two that is a multiple of sizeof(void*).
        let align = if align < size_of::<*const c_void>() { size_of::<*const c_void>() } else { align };
        let mut out: *mut c_void = core::ptr::null_mut();
        // SAFETY: `out` is a valid place to store the result; the call reads only its
        // numeric arguments.
        let rc = unsafe { posix_memalign(&mut out, align, size) };
        if rc == 0 {
            out as *mut u8
        } else {
            core::ptr::null_mut()
        }
    }

    /// Releases storage obtained from `raw_allocate`.
    ///
    /// # Safety
    /// `p` must come from `raw_allocate` and must not have been released.
    pub(crate) unsafe fn raw_deallocate(p: *mut u8) {
        if !p.is_null() {
            // SAFETY: per the function contract, `p` came from posix_memalign.
            unsafe { free(p as *mut c_void) }
        }
    }

    pub(crate) fn page_size() -> usize {
        // SAFETY: a plain query with no pointer arguments.
        let v = unsafe { sysconf(SC_PAGESIZE) };
        if v > 0 {
            v as usize
        } else {
            4096
        }
    }

    /// Reserves address space with no access rights. Null on failure.
    pub(crate) fn reserve_region(len: usize) -> *mut u8 {
        // SAFETY: anonymous mapping with a null hint; no existing memory is touched.
        let p = unsafe {
            mmap(
                core::ptr::null_mut(),
                len,
                PROT_NONE,
                MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE,
                -1,
                0,
            )
        };
        if p as isize == -1 {
            core::ptr::null_mut()
        } else {
            p as *mut u8
        }
    }

    /// Releases a region from `reserve_region`.
    ///
    /// # Safety
    /// `p` and `len` must describe a live reservation.
    pub(crate) unsafe fn release_region(p: *mut u8, len: usize) {
        // SAFETY: per the contract, the range is exactly a live mapping.
        unsafe {
            munmap(p as *mut c_void, len);
        }
    }

    /// Makes `[p, p + len)` readable and writable. Returns false on failure.
    ///
    /// # Safety
    /// The range must lie inside a live reservation, page aligned.
    pub(crate) unsafe fn commit_pages(p: *mut u8, len: usize) -> bool {
        // SAFETY: per the contract, the range is inside a live mapping.
        unsafe { mprotect(p as *mut c_void, len, PROT_READ | PROT_WRITE) == 0 }
    }

    /// Returns physical pages to the OS and forbids access. Returns false on failure.
    ///
    /// # Safety
    /// The range must lie inside a live reservation, page aligned.
    pub(crate) unsafe fn decommit_pages(p: *mut u8, len: usize) -> bool {
        // SAFETY: per the contract, the range is inside a live mapping.
        unsafe {
            madvise(p as *mut c_void, len, MADV_DONTNEED);
            mprotect(p as *mut c_void, len, PROT_NONE) == 0
        }
    }
}

#[cfg(windows)]
mod imp {
    use core::ffi::c_void;

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_DECOMMIT: u32 = 0x4000;
    const MEM_RELEASE: u32 = 0x8000;
    const PAGE_NOACCESS: u32 = 0x01;
    const PAGE_READWRITE: u32 = 0x04;

    #[repr(C)]
    struct SystemInfo {
        processor_architecture: u16,
        reserved: u16,
        page_size: u32,
        min_application_address: *mut c_void,
        max_application_address: *mut c_void,
        active_processor_mask: usize,
        number_of_processors: u32,
        processor_type: u32,
        allocation_granularity: u32,
        processor_level: u16,
        processor_revision: u16,
    }

    extern "system" {
        fn VirtualAlloc(addr: *mut c_void, size: usize, alloc_type: u32, protect: u32) -> *mut c_void;
        fn VirtualFree(addr: *mut c_void, size: usize, free_type: u32) -> i32;
        fn GetSystemInfo(info: *mut SystemInfo);
    }

    extern "C" {
        fn _aligned_malloc(size: usize, alignment: usize) -> *mut c_void;
        fn _aligned_free(ptr: *mut c_void);
    }

    pub(crate) fn raw_allocate(size: usize, align: usize) -> *mut u8 {
        if size == 0 {
            return core::ptr::null_mut();
        }
        // SAFETY: plain call; returns null on failure.
        unsafe { _aligned_malloc(size, align) as *mut u8 }
    }

    /// # Safety
    /// `p` must come from `raw_allocate` and must not have been released.
    pub(crate) unsafe fn raw_deallocate(p: *mut u8) {
        if !p.is_null() {
            // SAFETY: per the contract, `p` came from _aligned_malloc.
            unsafe { _aligned_free(p as *mut c_void) }
        }
    }

    pub(crate) fn page_size() -> usize {
        // SAFETY: SystemInfo is plain data; zero is a valid bit pattern for it.
        let mut info: SystemInfo = unsafe { core::mem::zeroed() };
        // SAFETY: `info` is a valid, writable SystemInfo.
        unsafe { GetSystemInfo(&mut info) };
        if info.page_size > 0 {
            info.page_size as usize
        } else {
            4096
        }
    }

    pub(crate) fn reserve_region(len: usize) -> *mut u8 {
        // SAFETY: reservation with a null hint; no existing memory is touched.
        unsafe { VirtualAlloc(core::ptr::null_mut(), len, MEM_RESERVE, PAGE_NOACCESS) as *mut u8 }
    }

    /// # Safety
    /// `p` must be the base of a live reservation.
    pub(crate) unsafe fn release_region(p: *mut u8, _len: usize) {
        // SAFETY: per the contract, `p` is the base of a reservation.
        unsafe {
            VirtualFree(p as *mut c_void, 0, MEM_RELEASE);
        }
    }

    /// # Safety
    /// The range must lie inside a live reservation.
    pub(crate) unsafe fn commit_pages(p: *mut u8, len: usize) -> bool {
        // SAFETY: per the contract, the range is inside a live reservation.
        unsafe { !VirtualAlloc(p as *mut c_void, len, MEM_COMMIT, PAGE_READWRITE).is_null() }
    }

    /// # Safety
    /// The range must lie inside a live reservation.
    pub(crate) unsafe fn decommit_pages(p: *mut u8, len: usize) -> bool {
        // SAFETY: per the contract, the range is inside a live reservation.
        unsafe { VirtualFree(p as *mut c_void, len, MEM_DECOMMIT) != 0 }
    }
}

pub(crate) use imp::{commit_pages, decommit_pages, page_size, raw_allocate, raw_deallocate, release_region, reserve_region};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_memory_is_aligned_and_releasable() {
        for align in [1usize, 8, 16, 64, 256, 4096] {
            let p = raw_allocate(100, align);
            assert!(!p.is_null());
            assert_eq!(p as usize & (align.max(8) - 1), 0);
            // SAFETY: `p` came from raw_allocate and is released exactly once.
            unsafe { raw_deallocate(p) };
        }
        assert!(raw_allocate(0, 16).is_null());
    }

    #[test]
    fn page_size_is_a_power_of_two() {
        let ps = page_size();
        assert!(ps >= 4096 && ps.is_power_of_two());
    }
}
pub mod memory;
pub mod lock;
pub mod shared;
pub mod huge;
