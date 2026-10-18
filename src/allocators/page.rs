//! Virtual-memory regions and a bump allocator over them.
//!
//! `VirtualRegion` reserves address space without touching physical memory; pages
//! become usable when committed and return to the OS when decommitted. Commit state is
//! tracked per page, so repeated or overlapping commits are idempotent.
//!
//! `PageAllocator` bumps through a region, committing pages lazily as it crosses them.
//! `reset` decommits everything. Individual frees are not supported.
//! Not `Sync`.

use core::cell::Cell;

use crate::foundation::align::{align_up, is_valid_alignment};
use crate::allocators::traits::Allocator;
use crate::structures::bitmap::Bitmap;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::MAX_BLOCK_SIZE;
use crate::foundation::error::{AllocError, AllocResult};
use crate::foundation::stats::Stats;
use crate::platform;

/// Reserved address space with per-page commit tracking.
pub struct VirtualRegion {
    base: *mut u8,
    reserved: usize,
    page: usize,
    committed: Bitmap,
}

// SAFETY: the region owns its mapping and has no thread affinity; it is not `Sync`.
unsafe impl Send for VirtualRegion {}

impl VirtualRegion {
    /// Reserves at least `bytes` (rounded up to whole pages). Nothing is committed.
    pub fn reserve(bytes: usize) -> AllocResult<Self> {
        if bytes == 0 {
            return Err(AllocError::InvalidArgument);
        }
        let page = platform::page_size();
        let total = align_up(bytes, page).ok_or(AllocError::Overflow)?;
        let base = platform::reserve_region(total);
        if base.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        match Bitmap::create(total / page) {
            Ok(committed) => Ok(VirtualRegion { base, reserved: total, page, committed }),
            Err(e) => {
                // SAFETY: `base` is the base of the reservation made above.
                unsafe { platform::release_region(base, total) };
                Err(e)
            }
        }
    }

    /// Commits the pages covering `[offset, offset + bytes)`.
    pub fn commit(&self, offset: usize, bytes: usize) -> AllocResult<()> {
        if self.base.is_null() || bytes == 0 {
            return Err(AllocError::InvalidArgument);
        }
        let end = offset.checked_add(bytes).ok_or(AllocError::InvalidArgument)?;
        if end > self.reserved {
            return Err(AllocError::InvalidArgument);
        }
        let first = offset / self.page;
        let last = end.div_ceil(self.page);
        for p in first..last {
            if self.committed.test(p) {
                continue;
            }
            // SAFETY: page `p` lies inside the live reservation (checked above).
            if !unsafe { platform::commit_pages(self.base.add(p * self.page), self.page) } {
                return Err(AllocError::OutOfMemory);
            }
            self.committed.set(p);
        }
        Ok(())
    }

    /// Decommits pages lying wholly inside `[offset, offset + bytes)`.
    pub fn decommit(&self, offset: usize, bytes: usize) -> AllocResult<()> {
        if self.base.is_null() || bytes == 0 {
            return Err(AllocError::InvalidArgument);
        }
        let end = offset.checked_add(bytes).ok_or(AllocError::InvalidArgument)?;
        if end > self.reserved {
            return Err(AllocError::InvalidArgument);
        }
        let first = offset.div_ceil(self.page);
        let last = end / self.page;
        for p in first..last {
            if !self.committed.test(p) {
                continue;
            }
            // SAFETY: page `p` lies inside the live reservation (checked above).
            unsafe { platform::decommit_pages(self.base.add(p * self.page), self.page) };
            self.committed.reset(p);
        }
        Ok(())
    }

    /// Start of the reservation (null only for an unusable region).
    pub fn base(&self) -> *mut u8 {
        self.base
    }

    /// Reserved length in bytes.
    pub fn reserved_bytes(&self) -> usize {
        self.reserved
    }

    /// Page size used by this region.
    pub fn page_size(&self) -> usize {
        self.page
    }

    /// Number of committed pages.
    pub fn committed_pages(&self) -> usize {
        self.committed.count()
    }

    /// Bytes currently committed.
    pub fn committed_bytes(&self) -> usize {
        self.committed.count() * self.page
    }

    /// True when page `index` is committed.
    pub fn is_page_committed(&self, index: usize) -> bool {
        index < self.committed.len() && self.committed.test(index)
    }
}

impl Drop for VirtualRegion {
    fn drop(&mut self) {
        if !self.base.is_null() {
            // SAFETY: `base` and `reserved` describe the reservation made in `reserve`.
            unsafe { platform::release_region(self.base, self.reserved) };
        }
    }
}

/// Bump allocator over lazily committed pages.
pub struct PageAllocator {
    region: VirtualRegion,
    top: Cell<usize>,
    stats: Cell<Stats>,
}

impl PageAllocator {
    /// Reserves `reserve_bytes` of address space; nothing is committed yet.
    pub fn create(reserve_bytes: usize) -> AllocResult<Self> {
        let region = VirtualRegion::reserve(reserve_bytes)?;
        let cap = region.reserved_bytes() as u64;
        Ok(PageAllocator {
            region,
            top: Cell::new(0),
            stats: Cell::new(Stats { capacity_bytes: cap, ..Stats::default() }),
        })
    }

    /// Bytes handed out so far.
    pub fn used_bytes(&self) -> usize {
        self.top.get()
    }

    /// Bytes of physical memory currently committed.
    pub fn committed_bytes(&self) -> usize {
        self.region.committed_bytes()
    }

    /// Reserved address space in bytes.
    pub fn reserved_bytes(&self) -> usize {
        self.region.reserved_bytes()
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    /// Releases everything and decommits every page the bump pointer touched.
    pub fn reset(&self) {
        let top = self.top.get();
        if top > 0 {
            if let Some(covered) = align_up(top, self.region.page_size()) {
                let _ = self.region.decommit(0, covered);
            }
        }
        let mut s = self.stats.get();
        s.deallocations += s.live_allocations();
        s.bytes_live = 0;
        self.stats.set(s);
        self.top.set(0);
    }

    fn fail(&self, e: AllocError) -> AllocError {
        let mut s = self.stats.get();
        s.on_failure();
        self.stats.set(s);
        e
    }
}

impl Allocator for PageAllocator {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !is_valid_alignment(align) {
            return Err(self.fail(AllocError::InvalidAlignment));
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        if size > MAX_BLOCK_SIZE {
            return Err(self.fail(AllocError::Overflow));
        }
        let base = self.region.base();
        if base.is_null() {
            return Err(self.fail(AllocError::CapacityExhausted));
        }
        let base_addr = base as usize;
        let aligned = match base_addr.checked_add(self.top.get()).and_then(|a| align_up(a, align)) {
            Some(a) => a,
            None => return Err(self.fail(AllocError::Overflow)),
        };
        let start = aligned - base_addr;
        let reserved = self.region.reserved_bytes();
        if start > reserved || size > reserved - start {
            return Err(self.fail(AllocError::CapacityExhausted));
        }
        if self.region.commit(start, size).is_err() {
            return Err(self.fail(AllocError::OutOfMemory));
        }
        self.top.set(start + size);
        let mut s = self.stats.get();
        s.on_alloc(size);
        self.stats.set(s);
        // SAFETY: `start + size <= reserved`, so the block lies inside the region.
        let p = unsafe { base.add(start) };
        Ok(Block::new(p, size, align))
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        if block.ptr().is_null() {
            return Err(AllocError::InvalidArgument);
        }
        Err(AllocError::NotSupported)
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE
            .union(Capabilities::RESET)
            .union(Capabilities::STATISTICS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_commit_is_idempotent_and_exact() {
        let r = VirtualRegion::reserve(1 << 20).unwrap();
        let page = r.page_size();
        assert_eq!(r.committed_pages(), 0);
        r.commit(100, 10000).unwrap();
        assert_eq!(r.committed_pages(), 3);
        r.commit(0, 4 * page).unwrap();
        assert_eq!(r.committed_pages(), 4);
        // SAFETY: byte 10 lies in committed page 0, which is writable.
        unsafe {
            *r.base().add(10) = 42;
            assert_eq!(*r.base().add(10), 42);
        }
        r.decommit(0, page).unwrap();
        assert!(!r.is_page_committed(0));
        assert!(r.is_page_committed(1));
    }

    #[test]
    fn region_rejects_bad_ranges() {
        let r = VirtualRegion::reserve(8192).unwrap();
        assert_eq!(r.commit(0, 0), Err(AllocError::InvalidArgument));
        assert_eq!(r.commit(8000, 1000), Err(AllocError::InvalidArgument));
        assert_eq!(VirtualRegion::reserve(0).err(), Some(AllocError::InvalidArgument));
    }

    #[test]
    fn page_allocator_commits_lazily_and_resets() {
        let p = PageAllocator::create(1 << 20).unwrap();
        assert_eq!(p.committed_bytes(), 0);
        let a = p.allocate(100, 64).unwrap();
        assert_eq!(a.ptr() as usize & 63, 0);
        let page = 4096;
        assert!(p.committed_bytes() >= page);
        // SAFETY: the block is committed and writable.
        unsafe { *a.ptr().add(99) = 7 };
        let big = p.allocate(3 * page, 16).unwrap();
        assert!(p.committed_bytes() >= 4 * page);
        // SAFETY: refused by design; nothing is touched.
        unsafe { assert_eq!(p.deallocate(a), Err(AllocError::NotSupported)) };
        let _ = big;
        p.reset();
        assert_eq!(p.used_bytes(), 0);
        assert_eq!(p.committed_bytes(), 0);
        assert_eq!(p.stats().live_allocations(), 0);
    }

    #[test]
    fn page_allocator_exhaustion() {
        let p = PageAllocator::create(8192).unwrap();
        p.allocate(8192, 16).unwrap();
        assert_eq!(p.allocate(1, 1).unwrap_err(), AllocError::CapacityExhausted);
    }
}
