//! Slab cache for one object size.
//!
//! Memory comes in 4 KiB slabs aligned to 4 KiB. Each slab begins with a header that
//! holds an occupancy bitmap, so `deallocate` finds the owning slab by masking the
//! pointer and then validates it exactly. Ownership is checked against this cache's own
//! slab list, so no memory outside a slab is ever read.
//!
//! Limits: object size 1..=2048 bytes, alignment at most 64, at most 256 objects per slab.
//! Empty slabs are kept until `release_empty` is called. Not `Sync`.

use core::cell::Cell;
use core::mem::{align_of, size_of};
use core::ptr;

use crate::foundation::align::{align_up, is_valid_alignment};
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::{DEBUG_CHECKS, FREED_BYTE, UNINIT_BYTE};
use crate::foundation::error::{AllocError, AllocResult};
use crate::foundation::stats::Stats;
use crate::platform;

const SLAB_BYTES: usize = 4096;
const MAX_OBJECT: usize = 2048;
const MAX_OBJECT_ALIGN: usize = 64;
const MAX_OBJECTS_PER_SLAB: usize = 256;
const SLAB_MAGIC: u64 = 0x5153_4C41_4248_4452; // "QSLABHDR"

#[repr(C)]
struct SlabHeader {
    magic: u64,
    next_all: *mut SlabHeader,
    next_partial: *mut SlabHeader,
    used: usize,
    occupancy: [u64; MAX_OBJECTS_PER_SLAB / 64],
}

/// Slab cache for objects of one size.
pub struct SlabAllocator {
    all: Cell<*mut SlabHeader>,
    partial: Cell<*mut SlabHeader>,
    object_size: usize,
    stride: usize,
    object_align: usize,
    per_slab: usize,
    slab_count: Cell<usize>,
    in_use: Cell<usize>,
    stats: Cell<Stats>,
}

// SAFETY: the cache owns its slabs and has no thread affinity; it is not `Sync`.
unsafe impl Send for SlabAllocator {}

fn objects_start(object_align: usize) -> usize {
    align_up(size_of::<SlabHeader>(), object_align).unwrap_or(size_of::<SlabHeader>())
}

impl SlabAllocator {
    /// A cache for objects of `object_size` bytes aligned to `object_align`.
    pub fn create(object_size: usize, object_align: usize) -> AllocResult<Self> {
        if object_size == 0 || object_size > MAX_OBJECT {
            return Err(AllocError::InvalidArgument);
        }
        if !is_valid_alignment(object_align) || object_align > MAX_OBJECT_ALIGN {
            return Err(AllocError::InvalidAlignment);
        }
        let align = object_align.max(align_of::<u64>());
        let stride = align_up(object_size, align).ok_or(AllocError::Overflow)?;
        let start = objects_start(align);
        let room = SLAB_BYTES.saturating_sub(start);
        let per_slab = (room / stride).min(MAX_OBJECTS_PER_SLAB);
        if per_slab == 0 {
            return Err(AllocError::InvalidArgument);
        }
        Ok(SlabAllocator {
            all: Cell::new(ptr::null_mut()),
            partial: Cell::new(ptr::null_mut()),
            object_size,
            stride,
            object_align: align,
            per_slab,
            slab_count: Cell::new(0),
            in_use: Cell::new(0),
            stats: Cell::new(Stats::default()),
        })
    }

    /// Objects per slab.
    pub fn objects_per_slab(&self) -> usize {
        self.per_slab
    }

    /// Slabs currently held.
    pub fn slab_count(&self) -> usize {
        self.slab_count.get()
    }

    /// Objects currently handed out.
    pub fn objects_in_use(&self) -> usize {
        self.in_use.get()
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    fn with_stats(&self, f: impl FnOnce(&mut Stats)) {
        let mut s = self.stats.get();
        f(&mut s);
        self.stats.set(s);
    }

    fn fail(&self, e: AllocError) -> AllocError {
        self.with_stats(|s| s.on_failure());
        e
    }

    /// The slab of this cache that contains `p`, if any. Reads only headers of this
    /// cache's own slabs.
    fn find_slab(&self, p: *const u8) -> Option<*mut SlabHeader> {
        let addr = p as usize;
        let mut s = self.all.get();
        while !s.is_null() {
            let base = s as usize;
            if addr >= base && addr < base + SLAB_BYTES {
                return Some(s);
            }
            // SAFETY: `s` is a live slab of this cache.
            s = unsafe { (*s).next_all };
        }
        None
    }

    /// True when `[p, p + n)` lies inside an object area of this cache's slabs.
    pub fn owns(&self, p: *const u8, n: usize) -> bool {
        match self.find_slab(p) {
            Some(s) => {
                let off = p as usize - s as usize;
                off >= objects_start(self.object_align) && n <= SLAB_BYTES - off
            }
            None => false,
        }
    }

    /// Frees every slab that holds no live objects. Returns how many were released.
    pub fn release_empty(&self) -> usize {
        let mut released = 0;
        let mut link: *mut *mut SlabHeader = self.all.as_ptr();
        // SAFETY: `link` always points at either `self.all` or a `next_all` field of a
        // live slab, so every dereference below is valid.
        unsafe {
            while !(*link).is_null() {
                let s = *link;
                if (*s).used == 0 {
                    *link = (*s).next_all;
                    (*s).magic = 0;
                    platform::raw_deallocate(s as *mut u8);
                    self.slab_count.set(self.slab_count.get() - 1);
                    released += 1;
                } else {
                    link = ptr::addr_of_mut!((*s).next_all);
                }
            }
        }
        // Rebuild the partial list from the survivors.
        self.partial.set(ptr::null_mut());
        let mut s = self.all.get();
        while !s.is_null() {
            // SAFETY: `s` is a live slab of this cache.
            unsafe {
                if (*s).used < self.per_slab {
                    (*s).next_partial = self.partial.get();
                    self.partial.set(s);
                }
                s = (*s).next_all;
            }
        }
        released
    }

    fn new_slab(&self) -> AllocResult<*mut SlabHeader> {
        let raw = platform::raw_allocate(SLAB_BYTES, SLAB_BYTES);
        if raw.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        let s = raw as *mut SlabHeader;
        // SAFETY: `raw` is a fresh SLAB_BYTES allocation, large enough for the header.
        unsafe {
            ptr::write(
                s,
                SlabHeader {
                    magic: SLAB_MAGIC,
                    next_all: self.all.get(),
                    next_partial: ptr::null_mut(),
                    used: 0,
                    occupancy: [0; MAX_OBJECTS_PER_SLAB / 64],
                },
            );
        }
        self.all.set(s);
        self.slab_count.set(self.slab_count.get() + 1);
        Ok(s)
    }
}

impl Drop for SlabAllocator {
    fn drop(&mut self) {
        let mut s = self.all.get();
        while !s.is_null() {
            // SAFETY: each slab was allocated by `new_slab` and is released exactly once here.
            unsafe {
                let next = (*s).next_all;
                platform::raw_deallocate(s as *mut u8);
                s = next;
            }
        }
    }
}

impl Allocator for SlabAllocator {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !is_valid_alignment(align) {
            return Err(self.fail(AllocError::InvalidAlignment));
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        if size > self.object_size {
            return Err(self.fail(AllocError::InvalidArgument));
        }
        if align > self.object_align {
            return Err(self.fail(AllocError::InvalidAlignment));
        }

        if self.partial.get().is_null() {
            let s = self.new_slab().map_err(|e| self.fail(e))?;
            self.partial.set(s);
        }
        let s = self.partial.get();
        // SAFETY: `s` is a live slab of this cache with at least one free object.
        let (index, obj) = unsafe {
            let mut index = 0usize;
            for (w, word) in (*s).occupancy.iter().enumerate() {
                if *word != u64::MAX {
                    index = w * 64 + (!*word).trailing_zeros() as usize;
                    break;
                }
            }
            (*s).occupancy[index / 64] |= 1u64 << (index % 64);
            (*s).used += 1;
            if (*s).used == self.per_slab {
                self.partial.set((*s).next_partial);
            }
            let obj = (s as *mut u8).add(objects_start(self.object_align) + index * self.stride);
            (index, obj)
        };
        let _ = index;
        if DEBUG_CHECKS {
            // SAFETY: the object lies inside the slab.
            unsafe { ptr::write_bytes(obj, UNINIT_BYTE, self.object_size) };
        }
        self.in_use.set(self.in_use.get() + 1);
        self.with_stats(|st| st.on_alloc(self.object_size));
        Ok(Block::new(obj, size, align))
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        if block.ptr().is_null() {
            return Err(AllocError::InvalidArgument);
        }
        if block.size() > self.object_size {
            return Err(AllocError::InvalidArgument);
        }
        let s = self.find_slab(block.ptr()).ok_or(AllocError::NotOwned)?;
        let start = objects_start(self.object_align);
        let rel = block.ptr() as usize - s as usize; if rel < start { return Err(AllocError::InvalidFree); } let off = rel - start;
        if off % self.stride != 0 {
            return Err(AllocError::InvalidFree);
        }
        let index = off / self.stride;
        if index >= self.per_slab {
            return Err(AllocError::InvalidFree);
        }
        let bit = 1u64 << (index % 64);
        // SAFETY: `s` is a live slab of this cache (found above).
        unsafe {
            if (*s).occupancy[index / 64] & bit == 0 {
                return Err(AllocError::DoubleFree);
            }
            let was_full = (*s).used == self.per_slab;
            (*s).occupancy[index / 64] &= !bit;
            (*s).used -= 1;
            if DEBUG_CHECKS {
                ptr::write_bytes(block.ptr(), FREED_BYTE, self.object_size);
            }
            if was_full {
                (*s).next_partial = self.partial.get();
                self.partial.set(s);
            }
        }
        self.in_use.set(self.in_use.get() - 1);
        self.with_stats(|st| st.on_free(self.object_size));
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE
            .union(Capabilities::INDIVIDUAL_FREE)
            .union(Capabilities::OWNERSHIP)
            .union(Capabilities::STATISTICS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocates_and_releases_empty_slabs() {
        let s = SlabAllocator::create(48, 16).unwrap();
        assert!(s.objects_per_slab() >= 2);
        let many = s.objects_per_slab() * 3 + 1;
        let mut held = Vec::new();
        for _ in 0..many {
            let b = s.allocate(48, 16).unwrap();
            assert_eq!(b.ptr() as usize & 15, 0);
            held.push(b);
        }
        assert_eq!(s.slab_count(), 4);
        for b in held.drain(..) {
            // SAFETY: each block came from `s` and is released once.
            unsafe { s.deallocate(b).unwrap() };
        }
        assert_eq!(s.objects_in_use(), 0);
        assert_eq!(s.release_empty(), 4);
        assert_eq!(s.slab_count(), 0);
    }

    #[test]
    fn misuse_is_refused() {
        let s = SlabAllocator::create(32, 8).unwrap();
        let a = s.allocate(32, 8).unwrap();
        let other = SlabAllocator::create(32, 8).unwrap();
        let ob = other.allocate(32, 8).unwrap();
        // SAFETY: each bad call is refused before any slab access; valid blocks are released once.
        unsafe {
            let shifted = Block::from_raw_parts(a.ptr().add(4), 4, 8);
            assert_eq!(s.deallocate(shifted), Err(AllocError::InvalidFree));
            assert_eq!(s.deallocate(ob), Err(AllocError::NotOwned));
            s.deallocate(a).unwrap();
            assert_eq!(s.deallocate(a), Err(AllocError::DoubleFree));
            other.deallocate(ob).unwrap();
        }
    }

    #[test]
    fn bad_configuration() {
        assert_eq!(SlabAllocator::create(0, 8).err(), Some(AllocError::InvalidArgument));
        assert_eq!(SlabAllocator::create(4096, 8).err(), Some(AllocError::InvalidArgument));
        assert_eq!(SlabAllocator::create(16, 128).err(), Some(AllocError::InvalidAlignment));
    }
}

impl crate::allocators::introspect::LiveBlocks for SlabAllocator {
    fn is_live(&self, block: Block) -> bool {
        if block.is_empty() || block.ptr().is_null() || block.size() > self.object_size {
            return false;
        }
        let Some(s) = self.find_slab(block.ptr()) else {
            return false;
        };
        let start = objects_start(self.object_align);
        let rel = block.ptr() as usize - s as usize;
        if rel < start {
            return false;
        }
        let off = rel - start;
        if off % self.stride != 0 {
            return false;
        }
        let index = off / self.stride;
        if index >= self.per_slab {
            return false;
        }
        let bit = 1u64 << (index % 64);
        // SAFETY: `s` is a slab of this cache, found above.
        unsafe { (*s).occupancy[index / 64] & bit != 0 }
    }
}
