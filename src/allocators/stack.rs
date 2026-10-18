//! LIFO allocator. Every allocation writes a header that records the state it
//! replaced, which gives exact validation:
//!
//! - `deallocate` accepts only the most recent live block (`OutOfOrderFree` otherwise).
//! - `rollback` walks the real allocation history. A checkpoint is valid only if its
//!   header is still on the chain and its serial number matches. A checkpoint whose
//!   block has been released and replaced is `StaleCheckpoint`, never silently accepted.
//!
//! Cost: one 56-byte header per allocation on 64-bit targets. Not `Sync`.

use core::cell::Cell;
use core::marker::PhantomData;
use core::mem::{size_of, MaybeUninit};
use core::ptr;

use crate::foundation::align::{align_up, is_valid_alignment};
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::{DEBUG_CHECKS, DEFAULT_ALIGNMENT, FREED_BYTE, MAX_BLOCK_SIZE, UNINIT_BYTE};
use crate::foundation::error::{AllocError, AllocResult};
use crate::foundation::stats::Stats;
use crate::platform;

const HEADER_MAGIC: u64 = 0x5153_4B53_4844_5231; // "QSKSHDR1"
const NO_HEADER: usize = usize::MAX;

#[repr(C)]
#[derive(Clone, Copy)]
struct Header {
    magic: u64,
    serial: u64,
    prev_serial: u64,
    size: usize,
    prev_top: usize,
    prev_header: usize,
    user_offset: usize,
}

/// A position a stack can be rolled back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackCheckpoint {
    top: usize,
    header: usize,
    serial: u64,
}

/// LIFO allocator with exact history validation.
pub struct StackAllocator<'a> {
    base: *mut u8,
    capacity: usize,
    top: Cell<usize>,
    high_water: Cell<usize>,
    last_header: Cell<usize>,
    last_serial: Cell<u64>,
    serial: Cell<u64>,
    stats: Cell<Stats>,
    owned: bool,
    _storage: PhantomData<&'a mut [MaybeUninit<u8>]>,
}

// SAFETY: the stack owns or exclusively borrows its region and has no thread affinity.
// It is not `Sync` because it contains `Cell`s.
unsafe impl Send for StackAllocator<'_> {}

impl<'a> StackAllocator<'a> {
    fn empty() -> Self {
        StackAllocator {
            base: ptr::null_mut(),
            capacity: 0,
            top: Cell::new(0),
            high_water: Cell::new(0),
            last_header: Cell::new(NO_HEADER),
            last_serial: Cell::new(0),
            serial: Cell::new(0),
            stats: Cell::new(Stats::default()),
            owned: false,
            _storage: PhantomData,
        }
    }

    /// A stack over caller-provided storage, borrowed for `'a`.
    pub fn from_slice(storage: &'a mut [MaybeUninit<u8>]) -> Self {
        let mut s = Self::empty();
        s.base = storage.as_mut_ptr() as *mut u8;
        s.capacity = storage.len();
        s.stats.set(Stats { capacity_bytes: storage.len() as u64, ..Stats::default() });
        s
    }

    fn with_stats<R>(&self, f: impl FnOnce(&mut Stats) -> R) -> R {
        let mut st = self.stats.get();
        let r = f(&mut st);
        self.stats.set(st);
        r
    }

    fn fail(&self, e: AllocError) -> AllocError {
        self.with_stats(|s| s.on_failure());
        e
    }

    /// Reads the header stored at `off`.
    ///
    /// # Safety
    /// `off` must be the offset of a header written by `allocate` and still on the chain.
    unsafe fn read_header(&self, off: usize) -> Header {
        // SAFETY: per the contract, a full `Header` lies at `off` inside the storage.
        unsafe { ptr::read_unaligned(self.base.add(off) as *const Header) }
    }

    /// Bytes handed out so far, including headers and padding.
    pub fn used(&self) -> usize {
        self.top.get()
    }

    /// Bytes still available.
    pub fn remaining(&self) -> usize {
        self.capacity - self.top.get()
    }

    /// Total capacity in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Largest value `used` has reached.
    pub fn high_water(&self) -> usize {
        self.high_water.get()
    }

    /// Number of live blocks.
    pub fn live_blocks(&self) -> u64 {
        self.stats.get().live_allocations()
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    /// True when `[p, p + n)` lies inside the stack's storage.
    pub fn owns(&self, p: *const u8, n: usize) -> bool {
        if self.base.is_null() || p.is_null() {
            return false;
        }
        let base = self.base as usize;
        let addr = p as usize;
        if addr < base {
            return false;
        }
        let offset = addr - base;
        if offset > self.capacity {
            return false;
        }
        n <= self.capacity - offset
    }

    /// Captures the current history position.
    pub fn checkpoint(&self) -> StackCheckpoint {
        StackCheckpoint {
            top: self.top.get(),
            header: self.last_header.get(),
            serial: self.last_serial.get(),
        }
    }

    /// Restores the state captured by `cp`, releasing everything above it.
    pub fn rollback(&self, cp: StackCheckpoint) -> AllocResult<()> {
        if cp.top > self.top.get() {
            return Err(AllocError::InvalidCheckpoint);
        }

        // Pass 1: walk the live chain down to the checkpoint without changing state.
        let mut hdr = self.last_header.get();
        while hdr != cp.header {
            if hdr == NO_HEADER {
                return Err(AllocError::InvalidCheckpoint);
            }
            if cp.header != NO_HEADER && hdr < cp.header {
                return Err(AllocError::InvalidCheckpoint);
            }
            // SAFETY: `hdr` is on the live chain, so a header was written there.
            let h = unsafe { self.read_header(hdr) };
            if h.magic != HEADER_MAGIC {
                return Err(AllocError::Corruption);
            }
            hdr = h.prev_header;
        }
        if cp.header != NO_HEADER {
            // SAFETY: `cp.header` was just reached on the live chain.
            let h = unsafe { self.read_header(cp.header) };
            if h.magic != HEADER_MAGIC {
                return Err(AllocError::Corruption);
            }
            if h.serial != cp.serial {
                return Err(AllocError::StaleCheckpoint);
            }
        } else if cp.serial != 0 {
            return Err(AllocError::StaleCheckpoint);
        }

        // Pass 2: commit. Release every block above the checkpoint, newest first.
        let old_top = self.top.get();
        let mut hdr = self.last_header.get();
        while hdr != cp.header {
            // SAFETY: validated by pass 1.
            let h = unsafe { self.read_header(hdr) };
            self.with_stats(|s| s.on_free(h.size));
            hdr = h.prev_header;
        }
        if DEBUG_CHECKS && old_top > cp.top {
            // SAFETY: `old_top <= capacity`, so the range lies inside the storage.
            unsafe { ptr::write_bytes(self.base.add(cp.top), FREED_BYTE, old_top - cp.top) };
        }
        self.top.set(cp.top);
        self.last_header.set(cp.header);
        self.last_serial.set(cp.serial);
        Ok(())
    }

    /// Releases everything.
    pub fn reset(&self) {
        let _ = self.rollback(StackCheckpoint { top: 0, header: NO_HEADER, serial: 0 });
    }
}

impl StackAllocator<'static> {
    /// A stack that owns `capacity` bytes of storage.
    pub fn new(capacity: usize) -> AllocResult<Self> {
        if capacity == 0 {
            return Ok(Self::empty());
        }
        if capacity > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }
        let p = platform::raw_allocate(capacity, DEFAULT_ALIGNMENT);
        if p.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        let mut s = Self::empty();
        s.base = p;
        s.capacity = capacity;
        s.owned = true;
        s.stats.set(Stats { capacity_bytes: capacity as u64, ..Stats::default() });
        Ok(s)
    }
}

impl Drop for StackAllocator<'_> {
    fn drop(&mut self) {
        if self.owned && !self.base.is_null() {
            // SAFETY: an owned stack's base came from `platform::raw_allocate` in `new`.
            unsafe { platform::raw_deallocate(self.base) };
        }
    }
}

impl Allocator for StackAllocator<'_> {
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
        if self.capacity == 0 {
            return Err(self.fail(AllocError::CapacityExhausted));
        }

        let base_addr = self.base as usize;
        let top = self.top.get();

        // Header: the first address at or after `top` that satisfies its alignment.
        let hdr_addr = match base_addr
            .checked_add(top)
            .and_then(|a| align_up(a, core::mem::align_of::<Header>()))
        {
            Some(a) => a,
            None => return Err(self.fail(AllocError::Overflow)),
        };
        let hdr_off = hdr_addr - base_addr;
        let hdr_end = match hdr_off.checked_add(size_of::<Header>()) {
            Some(e) if hdr_off <= self.capacity && e <= self.capacity => e,
            _ => return Err(self.fail(AllocError::CapacityExhausted)),
        };

        // User block: after the header, aligned as requested.
        let user_addr = match align_up(base_addr + hdr_end, align) {
            Some(a) => a,
            None => return Err(self.fail(AllocError::Overflow)),
        };
        let user_off = user_addr - base_addr;
        if user_off > self.capacity || size > self.capacity - user_off {
            return Err(self.fail(AllocError::CapacityExhausted));
        }

        let serial = self.serial.get() + 1;
        self.serial.set(serial);
        let header = Header {
            magic: HEADER_MAGIC,
            serial,
            prev_serial: self.last_serial.get(),
            size,
            prev_top: top,
            prev_header: self.last_header.get(),
            user_offset: user_off,
        };
        // SAFETY: `hdr_off..hdr_end` lies inside the storage (checked above).
        unsafe { ptr::write_unaligned(self.base.add(hdr_off) as *mut Header, header) };

        // SAFETY: `user_off + size <= capacity`, checked above.
        let p = unsafe { self.base.add(user_off) };
        if DEBUG_CHECKS {
            // SAFETY: the user block lies inside the storage.
            unsafe { ptr::write_bytes(p, UNINIT_BYTE, size) };
        }
        self.last_header.set(hdr_off);
        self.last_serial.set(serial);
        let new_top = user_off + size;
        self.top.set(new_top);
        if new_top > self.high_water.get() {
            self.high_water.set(new_top);
        }
        self.with_stats(|s| s.on_alloc(size));
        Ok(Block::new(p, size, align))
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        if block.ptr().is_null() {
            return Err(AllocError::InvalidArgument);
        }
        if !self.owns(block.ptr(), block.size()) {
            return Err(AllocError::NotOwned);
        }
        let off = block.ptr() as usize - self.base as usize;
        if off >= self.top.get() {
            return Err(AllocError::DoubleFree);
        }
        let last = self.last_header.get();
        if last == NO_HEADER {
            return Err(AllocError::InvalidFree);
        }
        // SAFETY: `last` is the newest header on the chain, written by `allocate`.
        let h = unsafe { self.read_header(last) };
        if h.magic != HEADER_MAGIC {
            return Err(AllocError::Corruption);
        }
        if h.user_offset != off {
            return Err(AllocError::OutOfOrderFree);
        }
        if h.size != block.size() {
            return Err(AllocError::InvalidArgument);
        }
        if DEBUG_CHECKS {
            // SAFETY: the block lies inside the storage (checked by `owns`).
            unsafe { ptr::write_bytes(block.ptr(), FREED_BYTE, h.size) };
        }
        self.top.set(h.prev_top);
        self.last_header.set(h.prev_header);
        self.last_serial.set(h.prev_serial);
        self.with_stats(|s| s.on_free(h.size));
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE
            .union(Capabilities::STACK_ORDER)
            .union(Capabilities::RESET)
            .union(Capabilities::ROLLBACK)
            .union(Capabilities::OWNERSHIP)
            .union(Capabilities::STATISTICS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage<const N: usize>() -> [MaybeUninit<u8>; N] {
        [MaybeUninit::uninit(); N]
    }

    #[test]
    fn lifo_free_succeeds_out_of_order_is_refused() {
        let mut buf = storage::<2048>();
        let s = StackAllocator::from_slice(&mut buf);
        let a = s.allocate(64, 16).unwrap();
        let b = s.allocate(32, 8).unwrap();
        // SAFETY: each block is released exactly once; the refused call leaves `a` live.
        unsafe {
            assert_eq!(s.deallocate(a), Err(AllocError::OutOfOrderFree));
            s.deallocate(b).unwrap();
            s.deallocate(a).unwrap();
        }
        assert_eq!(s.used(), 0);
        assert_eq!(s.live_blocks(), 0);
    }

    #[test]
    fn double_free_is_detected() {
        let mut buf = storage::<1024>();
        let s = StackAllocator::from_slice(&mut buf);
        let a = s.allocate(64, 16).unwrap();
        // SAFETY: the second call is the misuse under test; it is refused before any access.
        unsafe {
            s.deallocate(a).unwrap();
            assert_eq!(s.deallocate(a), Err(AllocError::DoubleFree));
        }
    }

    #[test]
    fn nested_checkpoints_roll_back() {
        let mut buf = storage::<4096>();
        let s = StackAllocator::from_slice(&mut buf);
        s.allocate(16, 8).unwrap();
        let outer = s.checkpoint();
        s.allocate(32, 8).unwrap();
        let inner = s.checkpoint();
        s.allocate(48, 8).unwrap();
        assert_eq!(s.live_blocks(), 3);
        s.rollback(inner).unwrap();
        assert_eq!(s.live_blocks(), 2);
        s.rollback(outer).unwrap();
        assert_eq!(s.live_blocks(), 1);
        assert_eq!(s.stats().bytes_live, 16);
    }

    #[test]
    fn replaced_block_makes_checkpoint_stale() {
        let mut buf = storage::<1024>();
        let s = StackAllocator::from_slice(&mut buf);
        let a = s.allocate(64, 8).unwrap();
        let cp = s.checkpoint();
        // SAFETY: `a` is the newest block and is released once here.
        unsafe { s.deallocate(a).unwrap() };
        s.allocate(64, 8).unwrap();
        assert_eq!(s.rollback(cp), Err(AllocError::StaleCheckpoint));
        assert_eq!(s.live_blocks(), 1);
    }

    #[test]
    fn checkpoint_above_top_is_invalid() {
        let mut buf = storage::<1024>();
        let s = StackAllocator::from_slice(&mut buf);
        s.allocate(8, 8).unwrap();
        let mid = s.checkpoint();
        s.allocate(8, 8).unwrap();
        let top = s.checkpoint();
        s.rollback(mid).unwrap();
        assert_eq!(s.rollback(top), Err(AllocError::InvalidCheckpoint));
    }

    #[test]
    fn reset_and_exhaustion() {
        let mut buf = storage::<512>();
        let s = StackAllocator::from_slice(&mut buf);
        s.allocate(200, 64).unwrap();
        assert_eq!(s.allocate(400, 8).unwrap_err(), AllocError::CapacityExhausted);
        s.reset();
        assert_eq!(s.used(), 0);
        assert_eq!(s.stats().bytes_live, 0);
        s.allocate(400, 8).unwrap();
    }

    #[test]
    fn owned_stack_works() {
        let s = StackAllocator::new(1024).unwrap();
        let b = s.allocate(100, 16).unwrap();
        assert_eq!(b.ptr() as usize & 15, 0);
        // SAFETY: `b` is the only live block and is released once.
        unsafe { s.deallocate(b).unwrap() };
    }
}
