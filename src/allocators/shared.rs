//! Shared-memory arena (feature 5, allocator side). A bump allocator whose state lives inside a named shared
//! region, so several processes can allocate from the same arena concurrently.
//!
//! The region starts with a 64-byte header: a magic value, the capacity, and the top offset. Allocation
//! reserves its range with a compare-and-swap on the top offset, so no process needs a lock. Blocks are
//! not freed one by one; the arena is reclaimed as a whole when the region is deleted.

use crate::decorators::Ownership;
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use crate::platform::shared::SharedMapping;
use std::sync::atomic::{AtomicU64, Ordering};

const MAGIC: u64 = 0x5153_4152_454E_4131;
const HEADER: usize = 64;

#[repr(C)]
struct Header {
    magic: AtomicU64,
    capacity: AtomicU64,
    top: AtomicU64,
}

/// An arena in a named shared-memory region.
pub struct SharedArena {
    mapping: SharedMapping,
    capacity: usize,
}

impl SharedArena {
    /// Creates the named arena with `capacity` bytes of data, or joins it if it already exists with the
    /// same capacity.
    pub fn create(name: &str, capacity: usize) -> AllocResult<Self> {
        let mapping = SharedMapping::create(name, HEADER + capacity).ok_or(AllocError::OutOfMemory)?;
        let arena = SharedArena { mapping, capacity };
        let header = arena.header();
        if header.magic.load(Ordering::Acquire) == MAGIC {
            if header.capacity.load(Ordering::Relaxed) != capacity as u64 {
                return Err(AllocError::InvalidArgument);
            }
        } else {
            header.capacity.store(capacity as u64, Ordering::Relaxed);
            header.top.store(0, Ordering::Relaxed);
            header.magic.store(MAGIC, Ordering::Release);
        }
        Ok(arena)
    }

    /// Joins an existing named arena. Fails if the name is unknown or the capacity differs.
    pub fn open(name: &str, capacity: usize) -> AllocResult<Self> {
        let mapping = SharedMapping::open(name, HEADER + capacity).ok_or(AllocError::NotOwned)?;
        let arena = SharedArena { mapping, capacity };
        let header = arena.header();
        if header.magic.load(Ordering::Acquire) != MAGIC || header.capacity.load(Ordering::Relaxed) != capacity as u64 {
            return Err(AllocError::InvalidArgument);
        }
        Ok(arena)
    }

    fn header(&self) -> &Header {
        // SAFETY: the mapping starts with a `Header` and is at least `HEADER` bytes long; the header is
        // accessed only through atomics.
        unsafe { &*(self.mapping.as_ptr() as *const Header) }
    }

    fn data(&self) -> *mut u8 {
        // SAFETY: `HEADER` bytes lie inside the mapping, so the data start is inside it too.
        unsafe { self.mapping.as_ptr().add(HEADER) }
    }

    /// Bytes reserved so far, across every process using the arena.
    pub fn used(&self) -> usize {
        self.header().top.load(Ordering::Acquire) as usize
    }

    /// Size of the data area in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl Allocator for SharedArena {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !align.is_power_of_two() {
            return Err(AllocError::InvalidAlignment);
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        let data = self.data() as usize;
        let header = self.header();
        loop {
            let top = header.top.load(Ordering::Acquire) as usize;
            let aligned = (data + top).next_multiple_of(align);
            let new_top = (aligned - data).checked_add(size).ok_or(AllocError::Overflow)?;
            if new_top > self.capacity {
                return Err(AllocError::CapacityExhausted);
            }
            if header
                .top
                .compare_exchange(top as u64, new_top as u64, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                // SAFETY: the range `[aligned, aligned + size)` was reserved exclusively by the compare-and-swap
                // and lies inside the data area.
                return Ok(unsafe { Block::from_raw_parts(aligned as *mut u8, size, align) });
            }
        }
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            Ok(())
        } else {
            Err(AllocError::NotSupported)
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE.union(Capabilities::OWNERSHIP)
    }
}

impl Ownership for SharedArena {
    fn owns(&self, p: *const u8, n: usize) -> bool {
        let data = self.data() as usize;
        let addr = p as usize;
        addr >= data && addr.checked_add(n).map_or(false, |end| end <= data + self.capacity)
    }
}
