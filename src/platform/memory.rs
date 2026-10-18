//! System memory queries (feature 40): physical memory totals and what is available right now.

use crate::platform;

/// A snapshot of system memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryInfo {
    pub total_physical: u64,
    pub available_physical: u64,
    pub page_size: usize,
    pub large_page_size: usize,
}

#[cfg(windows)]
mod imp {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        memory_load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }

    extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
        fn GetLargePageMinimum() -> usize;
    }

    pub fn query() -> Option<(u64, u64, usize)> {
        let mut status = MemoryStatusEx {
            length: core::mem::size_of::<MemoryStatusEx>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        // SAFETY: `status` is a valid, correctly sized MEMORYSTATUSEX with `length` set as the API requires.
        if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
            return None;
        }
        // SAFETY: plain query with no arguments.
        let large = unsafe { GetLargePageMinimum() };
        Some((status.total_phys, status.avail_phys, large))
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use core::ffi::c_long;

    extern "C" {
        fn sysconf(name: i32) -> c_long;
    }

    const SC_PAGESIZE: i32 = 30;
    const SC_PHYS_PAGES: i32 = 85;
    const SC_AVPHYS_PAGES: i32 = 86;

    pub fn query() -> Option<(u64, u64, usize)> {
        // SAFETY: `sysconf` is a read-only query for a documented name.
        let (page, total, avail) = unsafe { (sysconf(SC_PAGESIZE), sysconf(SC_PHYS_PAGES), sysconf(SC_AVPHYS_PAGES)) };
        if page <= 0 || total < 0 || avail < 0 {
            return None;
        }
        let page = page as u64;
        Some((total as u64 * page, avail as u64 * page, 2 * 1024 * 1024))
    }
}

/// Reads the current memory situation of the machine. `None` if the OS does not answer.
pub fn system_memory() -> Option<MemoryInfo> {
    let (total_physical, available_physical, large) = imp::query()?;
    Some(MemoryInfo {
        total_physical,
        available_physical,
        page_size: platform::page_size(),
        large_page_size: large,
    })
}
