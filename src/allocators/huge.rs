//! Large-page arena (feature 6, allocator side). A bump allocator over a reservation backed by huge pages
//! when the OS allows it. `page_kind` reports which pages were obtained, so callers can see whether the
//! large-page path worked on the current machine.

use crate::allocators::traits::Allocator;
use crate::decorators::Ownership;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use crate::platform::huge::{PageKind, Reservation};
use core::cell::Cell;

/// A bump arena over a huge-page reservation.
pub struct HugeArena {
    reservation: Reservation,
    top: Cell<usize>,
}

impl HugeArena {
    /// Reserves at least `capacity` bytes, preferring large pages.
    pub fn new(capacity: usize) -> AllocResult<Self> {
        let reservation = Reservation::reserve(capacity).ok_or(AllocError::OutOfMemory)?;
        Ok(HugeArena { reservation, top: Cell::new(0) })
    }

    /// Which pages back the arena.
    pub fn page_kind(&self) -> PageKind {
        self.reservation.kind()
    }

    /// Bytes handed out so far.
    pub fn used(&self) -> usize {
        self.top.get()
    }

    /// Size of the reservation in bytes.
    pub fn capacity(&self) -> usize {
        self.reservation.len()
    }
}

impl Allocator for HugeArena {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !align.is_power_of_two() {
            return Err(AllocError::InvalidAlignment);
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        let base = self.reservation.as_ptr() as usize;
        let aligned = (base + self.top.get()).next_multiple_of(align);
        let new_top = (aligned - base).checked_add(size).ok_or(AllocError::Overflow)?;
        if new_top > self.reservation.len() {
            return Err(AllocError::CapacityExhausted);
        }
        self.top.set(new_top);
        // SAFETY: `[aligned, aligned + size)` lies inside the reservation, which this arena owns.
        Ok(unsafe { Block::from_raw_parts(aligned as *mut u8, size, align) })
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

impl Ownership for HugeArena {
    fn owns(&self, p: *const u8, n: usize) -> bool {
        let base = self.reservation.as_ptr() as usize;
        let addr = p as usize;
        addr >= base && addr.checked_add(n).map_or(false, |end| end <= base + self.reservation.len())
    }
}
