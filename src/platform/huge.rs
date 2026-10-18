//! Large pages (feature 6): reserves memory backed by huge pages when the OS allows it. Otherwise the
//! reservation falls back to normal pages, and `PageKind` says which one was obtained.
//!
//! Windows large pages need the "Lock pages in memory" privilege. Without it the call fails and the
//! normal-page path is used. Linux uses `MAP_HUGETLB`, which needs huge pages reserved by the system.

use crate::platform;

/// The kind of pages behind a reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    Large,
    Normal,
}

#[cfg(windows)]
mod imp {
    use super::PageKind;
    use core::ffi::c_void;

    const MEM_COMMIT: u32 = 0x1000;
    const MEM_RESERVE: u32 = 0x2000;
    const MEM_RELEASE: u32 = 0x8000;
    const MEM_LARGE_PAGES: u32 = 0x2000_0000;
    const PAGE_READWRITE: u32 = 0x04;

    extern "system" {
        fn VirtualAlloc(address: *mut c_void, size: usize, alloc_type: u32, protect: u32) -> *mut c_void;
        fn VirtualFree(address: *mut c_void, size: usize, free_type: u32) -> i32;
        fn GetLargePageMinimum() -> usize;
    }

    pub fn reserve(min_len: usize, normal_page: usize) -> Option<(*mut u8, usize, PageKind)> {
        // SAFETY: plain query with no arguments.
        let large = if super::enable_large_pages() { unsafe { GetLargePageMinimum() } } else { 0 };
        if large != 0 {
            let len = min_len.div_ceil(large) * large;
            // SAFETY: a new reservation at a system-chosen address; the result is checked for null.
            let p = unsafe { VirtualAlloc(core::ptr::null_mut(), len, MEM_RESERVE | MEM_COMMIT | MEM_LARGE_PAGES, PAGE_READWRITE) };
            if !p.is_null() {
                return Some((p as *mut u8, len, PageKind::Large));
            }
        }
        let len = min_len.div_ceil(normal_page) * normal_page;
        // SAFETY: as above.
        let p = unsafe { VirtualAlloc(core::ptr::null_mut(), len, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
        if p.is_null() {
            None
        } else {
            Some((p as *mut u8, len, PageKind::Normal))
        }
    }

    pub fn release(p: *mut u8, _len: usize) {
        // SAFETY: `p` came from `VirtualAlloc` and is released once, here.
        unsafe { VirtualFree(p as *mut c_void, 0, MEM_RELEASE) };
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::PageKind;
    use core::ffi::{c_int, c_void};

    const PROT_READ_WRITE: c_int = 3;
    const MAP_PRIVATE: c_int = 0x02;
    const MAP_ANONYMOUS: c_int = 0x20;
    const MAP_HUGETLB: c_int = 0x40000;
    const HUGE_PAGE: usize = 2 * 1024 * 1024;

    extern "C" {
        fn mmap(address: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, offset: i64) -> *mut c_void;
        fn munmap(address: *mut c_void, len: usize) -> c_int;
    }

    pub fn reserve(min_len: usize, normal_page: usize) -> Option<(*mut u8, usize, PageKind)> {
        let huge_len = min_len.div_ceil(HUGE_PAGE) * HUGE_PAGE;
        // SAFETY: an anonymous mapping at a system-chosen address; the result is checked for failure.
        let p = unsafe {
            mmap(core::ptr::null_mut(), huge_len, PROT_READ_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB, -1, 0)
        };
        if p as isize != -1 {
            return Some((p as *mut u8, huge_len, PageKind::Large));
        }
        let len = min_len.div_ceil(normal_page) * normal_page;
        // SAFETY: as above, without huge pages.
        let p = unsafe { mmap(core::ptr::null_mut(), len, PROT_READ_WRITE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0) };
        if p as isize == -1 {
            None
        } else {
            Some((p as *mut u8, len, PageKind::Normal))
        }
    }

    pub fn release(p: *mut u8, len: usize) {
        // SAFETY: `p` is a mapping of `len` bytes from `reserve`, unmapped once, here.
        unsafe { munmap(p as *mut c_void, len) };
    }
}

/// A committed, zeroed region of at least the requested size.
pub struct Reservation {
    ptr: *mut u8,
    len: usize,
    kind: PageKind,
}

impl Reservation {
    /// Reserves at least `min_len` bytes, preferring large pages. Returns `None` when even normal
    /// pages cannot be obtained.
    pub fn reserve(min_len: usize) -> Option<Self> {
        if min_len == 0 {
            return None;
        }
        let (ptr, len, kind) = imp::reserve(min_len, platform::page_size())?;
        Some(Reservation { ptr, len, kind })
    }

    /// Start of the region.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// Length of the region in bytes (rounded up to the page size used).
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the region is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Which pages back the region.
    pub fn kind(&self) -> PageKind {
        self.kind
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        imp::release(self.ptr, self.len);
    }
}

#[cfg(windows)]
fn enable_large_pages() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(privilege::enable_lock_memory)
}

#[cfg(windows)]
mod privilege {
    use core::ffi::c_void;

    #[repr(C)]
    struct Luid {
        low: u32,
        high: i32,
    }

    #[repr(C)]
    struct LuidAndAttributes {
        luid: Luid,
        attributes: u32,
    }

    #[repr(C)]
    struct TokenPrivileges {
        count: u32,
        privilege: LuidAndAttributes,
    }

    const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
    const TOKEN_QUERY: u32 = 0x0008;
    const SE_PRIVILEGE_ENABLED: u32 = 0x0002;
    const ERROR_SUCCESS: u32 = 0;

    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: *mut c_void, access: u32, token: *mut *mut c_void) -> i32;
        fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
        fn AdjustTokenPrivileges(
            token: *mut c_void,
            disable_all: i32,
            state: *const TokenPrivileges,
            length: u32,
            previous: *mut c_void,
            returned: *mut u32,
        ) -> i32;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    /// Enables SeLockMemoryPrivilege for this process. Returns false when the account does not hold it.
    pub fn enable_lock_memory() -> bool {
        let name: Vec<u16> = "SeLockMemoryPrivilege".encode_utf16().chain(core::iter::once(0)).collect();
        let mut token: *mut c_void = core::ptr::null_mut();
        let mut luid = Luid { low: 0, high: 0 };
        // SAFETY: each call receives valid pointers to locals that outlive it; the token is closed once.
        unsafe {
            if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token) == 0 {
                return false;
            }
            let mut enabled = false;
            if LookupPrivilegeValueW(core::ptr::null(), name.as_ptr(), &mut luid) != 0 {
                let state = TokenPrivileges {
                    count: 1,
                    privilege: LuidAndAttributes { luid, attributes: SE_PRIVILEGE_ENABLED },
                };
                let adjusted =
                    AdjustTokenPrivileges(token, 0, &state, 0, core::ptr::null_mut(), core::ptr::null_mut()) != 0;
                // AdjustTokenPrivileges reports success even when the privilege is not held; GetLastError tells.
                enabled = adjusted && GetLastError() == ERROR_SUCCESS;
            }
            CloseHandle(token);
            enabled
        }
    }
}
