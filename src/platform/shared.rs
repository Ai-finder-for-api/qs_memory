//! Named shared memory (feature 5): a block of memory several processes can map by name.
//!
//! Windows uses a file mapping in the session-local namespace. Linux uses `shm_open` with a `/`-prefixed
//! name. Every process that maps the same name sees the same bytes.

#[cfg(windows)]
mod imp {
    use core::ffi::c_void;

    const PAGE_READWRITE: u32 = 0x04;
    const FILE_MAP_ALL_ACCESS: u32 = 0x000F_001F;

    extern "system" {
        fn CreateFileMappingW(
            file: *mut c_void,
            attributes: *mut c_void,
            protect: u32,
            size_high: u32,
            size_low: u32,
            name: *const u16,
        ) -> *mut c_void;
        fn OpenFileMappingW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
        fn MapViewOfFile(mapping: *mut c_void, access: u32, offset_high: u32, offset_low: u32, bytes: usize) -> *mut c_void;
        fn UnmapViewOfFile(base: *const c_void) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    fn wide(name: &str) -> Vec<u16> {
        format!("Local\\{name}").encode_utf16().chain(core::iter::once(0)).collect()
    }

    fn view(handle: *mut c_void, len: usize) -> Option<(*mut u8, usize)> {
        // SAFETY: `handle` is a valid file-mapping handle returned by the create or open call.
        let base = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, len) };
        if base.is_null() {
            // SAFETY: the handle is valid and is closed exactly once here.
            unsafe { CloseHandle(handle) };
            None
        } else {
            Some((base as *mut u8, handle as usize))
        }
    }

    pub fn create(name: &str, len: usize) -> Option<(*mut u8, usize)> {
        let wide_name = wide(name);
        // SAFETY: the name is null-terminated UTF-16 and the size split is exact for `len`.
        let handle = unsafe {
            CreateFileMappingW(
                usize::MAX as *mut c_void,
                core::ptr::null_mut(),
                PAGE_READWRITE,
                (len as u64 >> 32) as u32,
                len as u32,
                wide_name.as_ptr(),
            )
        };
        if handle.is_null() {
            None
        } else {
            view(handle, len)
        }
    }

    pub fn open(name: &str, len: usize) -> Option<(*mut u8, usize)> {
        let wide_name = wide(name);
        // SAFETY: the name is null-terminated UTF-16.
        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, wide_name.as_ptr()) };
        if handle.is_null() {
            None
        } else {
            view(handle, len)
        }
    }

    pub fn unmap(ptr: *mut u8, _len: usize, handle: usize) {
        // SAFETY: `ptr` is a view from `create` or `open`, and `handle` is its mapping, released once here.
        unsafe {
            UnmapViewOfFile(ptr as *const c_void);
            CloseHandle(handle as *mut c_void);
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use core::ffi::{c_char, c_int, c_void};

    const O_RDWR: c_int = 2;
    const O_CREAT: c_int = 0o100;
    const PROT_READ_WRITE: c_int = 3;
    const MAP_SHARED: c_int = 1;

    extern "C" {
        fn shm_open(name: *const c_char, oflag: c_int, mode: u32) -> c_int;
        fn ftruncate(fd: c_int, len: i64) -> c_int;
        fn mmap(address: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, offset: i64) -> *mut c_void;
        fn munmap(address: *mut c_void, len: usize) -> c_int;
        fn close(fd: c_int) -> c_int;
    }

    fn cname(name: &str) -> Vec<u8> {
        let mut v = vec![b'/'];
        v.extend_from_slice(name.as_bytes());
        v.push(0);
        v
    }

    fn map(fd: c_int, len: usize) -> Option<(*mut u8, usize)> {
        // SAFETY: `fd` is an open shared-memory descriptor of at least `len` bytes.
        let base = unsafe { mmap(core::ptr::null_mut(), len, PROT_READ_WRITE, MAP_SHARED, fd, 0) };
        // SAFETY: the mapping keeps the memory alive, so the descriptor can be closed now.
        unsafe { close(fd) };
        if base as isize == -1 {
            None
        } else {
            Some((base as *mut u8, 0))
        }
    }

    pub fn create(name: &str, len: usize) -> Option<(*mut u8, usize)> {
        let c = cname(name);
        // SAFETY: `c` is a null-terminated name.
        let fd = unsafe { shm_open(c.as_ptr() as *const c_char, O_RDWR | O_CREAT, 0o600) };
        if fd < 0 {
            return None;
        }
        // SAFETY: `fd` is open.
        if unsafe { ftruncate(fd, len as i64) } != 0 {
            // SAFETY: `fd` is open and closed once here.
            unsafe { close(fd) };
            return None;
        }
        map(fd, len)
    }

    pub fn open(name: &str, len: usize) -> Option<(*mut u8, usize)> {
        let c = cname(name);
        // SAFETY: `c` is a null-terminated name.
        let fd = unsafe { shm_open(c.as_ptr() as *const c_char, O_RDWR, 0o600) };
        if fd < 0 {
            None
        } else {
            map(fd, len)
        }
    }

    pub fn unmap(ptr: *mut u8, len: usize, _handle: usize) {
        // SAFETY: `ptr` is a mapping of `len` bytes from `create` or `open`, unmapped once here.
        unsafe { munmap(ptr as *mut c_void, len) };
    }
}

/// A mapped view of named shared memory. The view is unmapped on drop.
pub struct SharedMapping {
    ptr: *mut u8,
    len: usize,
    handle: usize,
}

impl SharedMapping {
    /// Creates the named region if needed and maps `len` bytes of it.
    pub fn create(name: &str, len: usize) -> Option<Self> {
        let (ptr, handle) = imp::create(name, len)?;
        Some(SharedMapping { ptr, len, handle })
    }

    /// Maps an existing named region of `len` bytes.
    pub fn open(name: &str, len: usize) -> Option<Self> {
        let (ptr, handle) = imp::open(name, len)?;
        Some(SharedMapping { ptr, len, handle })
    }

    /// Start of the mapped bytes.
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// Number of mapped bytes.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when the mapping is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Drop for SharedMapping {
    fn drop(&mut self) {
        imp::unmap(self.ptr, self.len, self.handle);
    }
}
