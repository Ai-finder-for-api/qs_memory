//! General-purpose variable-size allocator over one region.
//!
//! Layout of every block (offsets are multiples of 16 from a 16-aligned base):
//! `[ header 48 | back record 16 | payload ... | footer 8 ]`
//! - header: magic, total block size, requested size, free-list links, state;
//! - back record: `{tag, offset}` just before the payload. `tag = BACK_TAG ^ offset`,
//!   so an interior or foreign pointer fails validation with high probability;
//! - footer: total block size, so the previous block is found when coalescing.
//!
//! Policy: first fit over an address-ordered free list. Freed blocks coalesce with
//! both physical neighbours at once, so no two free blocks are adjacent. `verify`
//! checks every structural invariant in O(blocks).
//!
//! 64-bit targets only (the header layout is asserted at compile time).
//! Not `Sync`.

use core::cell::Cell;
use core::mem::size_of;
use core::ptr;

use crate::foundation::align::{align_up, is_valid_alignment};
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::{DEBUG_CHECKS, FREED_BYTE, MAX_BLOCK_SIZE, UNINIT_BYTE};
use crate::foundation::error::{AllocError, AllocResult};
use crate::foundation::stats::Stats;
use crate::platform;

const BLOCK_MAGIC: u64 = 0x5153_4C46_5245_454B; // "QSLFREEK"
const BACK_TAG: u64 = 0xB4C7_A9D1_5C0F_FEE5;
const STATE_ALLOCATED: u64 = 0;
const STATE_FREE: u64 = 1;
const GRANULE: usize = 16;
const HEADER_SIZE: usize = 48;
const BACK_RECORD: usize = 16;
const FOOTER: usize = 8;
const MIN_BLOCK: usize = 64;
const NONE: usize = usize::MAX;

#[repr(C)]
#[derive(Clone, Copy)]
struct Header {
    magic: u64,
    size: usize,
    requested: usize,
    next_free: usize,
    prev_free: usize,
    state: u64,
}

const _: () = assert!(size_of::<Header>() == HEADER_SIZE, "free-list header layout");

/// Structural snapshot of a free list heap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FreeListFragmentation {
    /// Total bytes in free blocks.
    pub free_bytes: usize,
    /// Size of the largest free block.
    pub largest_free_block: usize,
    /// Number of free blocks.
    pub free_block_count: usize,
    /// Number of allocated blocks.
    pub allocated_block_count: usize,
}

impl FreeListFragmentation {
    /// 0.0 when all free memory is one block; approaches 1.0 as it scatters.
    pub fn external_fragmentation(&self) -> f64 {
        if self.free_bytes == 0 {
            0.0
        } else {
            1.0 - self.largest_free_block as f64 / self.free_bytes as f64
        }
    }
}

/// Coalescing first-fit heap over one owned region.
pub struct FreeListAllocator {
    storage: *mut u8,
    capacity: usize,
    head: Cell<usize>,
    stats: Cell<Stats>,
}

// SAFETY: the heap owns its region and has no thread affinity; it is not `Sync`.
unsafe impl Send for FreeListAllocator {}

impl FreeListAllocator {
    /// Creates a heap over `capacity` bytes, rounded down to 16. Requires at least 64 bytes.
    pub fn create(capacity: usize) -> AllocResult<Self> {
        let cap = capacity & !(GRANULE - 1);
        if cap < MIN_BLOCK {
            return Err(AllocError::InvalidArgument);
        }
        if cap > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }
        let storage = platform::raw_allocate(cap, GRANULE);
        if storage.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        let heap = FreeListAllocator {
            storage,
            capacity: cap,
            head: Cell::new(0),
            stats: Cell::new(Stats { capacity_bytes: cap as u64, ..Stats::default() }),
        };
        // SAFETY: the region is `cap >= 64` bytes and 16-aligned; the header and footer
        // lie inside it.
        unsafe {
            heap.put_header(0, Header {
                magic: BLOCK_MAGIC,
                size: cap,
                requested: 0,
                next_free: NONE,
                prev_free: NONE,
                state: STATE_FREE,
            });
            heap.put_footer(0, cap);
        }
        Ok(heap)
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

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    /// Heap size in bytes.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// True when `[p, p + n)` lies inside the heap.
    pub fn owns(&self, p: *const u8, n: usize) -> bool {
        if self.storage.is_null() || p.is_null() {
            return false;
        }
        let base = self.storage as usize;
        let addr = p as usize;
        if addr < base {
            return false;
        }
        let offset = addr - base;
        if offset >= self.capacity {
            return false;
        }
        n <= self.capacity - offset
    }

    /// Reads the header of the block starting at `off`.
    ///
    /// # Safety
    /// `off` must be the start of a block (the header must have been written).
    unsafe fn header(&self, off: usize) -> Header {
        // SAFETY: per the contract, a header lies at `off` inside the region.
        unsafe { ptr::read_unaligned(self.storage.add(off) as *const Header) }
    }

    /// # Safety
    /// `off..off + HEADER_SIZE` must lie inside the region.
    unsafe fn put_header(&self, off: usize, h: Header) {
        // SAFETY: per the contract, the header bytes are inside the region.
        unsafe { ptr::write_unaligned(self.storage.add(off) as *mut Header, h) };
    }

    /// # Safety
    /// `end` must be the end offset of a block of `total` bytes starting at `end - total`.
    unsafe fn put_footer(&self, off: usize, total: usize) {
        // SAFETY: the footer `off + total - 8` lies inside the block.
        unsafe { ptr::write_unaligned(self.storage.add(off + total - FOOTER) as *mut usize, total) };
    }

    /// # Safety
    /// `end` must be the end offset of some block (so 8 bytes precede it).
    unsafe fn footer(&self, end: usize) -> usize {
        // SAFETY: per the contract, the footer `end - 8` lies inside the region.
        unsafe { ptr::read_unaligned(self.storage.add(end - FOOTER) as *const usize) }
    }

    /// # Safety
    /// `off` must start a free block.
    unsafe fn set_next(&self, off: usize, next: usize) {
        // SAFETY: per the contract, `off` starts a block with a header.
        unsafe {
            let mut h = self.header(off);
            h.next_free = next;
            self.put_header(off, h);
        }
    }

    /// # Safety
    /// `off` must start a free block.
    unsafe fn set_prev(&self, off: usize, prev: usize) {
        // SAFETY: per the contract, `off` starts a block with a header.
        unsafe {
            let mut h = self.header(off);
            h.prev_free = prev;
            self.put_header(off, h);
        }
    }

    /// Removes a free block from the free list.
    ///
    /// # Safety
    /// `off` must start a free block that is on the list.
    unsafe fn unlink_free(&self, off: usize) {
        // SAFETY: per the contract, `off` starts a free block on the list.
        unsafe {
            let h = self.header(off);
            if h.prev_free == NONE {
                self.head.set(h.next_free);
            } else {
                self.set_next(h.prev_free, h.next_free);
            }
            if h.next_free != NONE {
                self.set_prev(h.next_free, h.prev_free);
            }
            let mut cleared = h;
            cleared.next_free = NONE;
            cleared.prev_free = NONE;
            self.put_header(off, cleared);
        }
    }

    /// Inserts a free block (already written with `STATE_FREE`) in address order.
    ///
    /// # Safety
    /// `off` must start a free block whose header has been written.
    unsafe fn insert_free_sorted(&self, off: usize) {
        let mut prev = NONE;
        let mut cur = self.head.get();
        // SAFETY: every node on the list is a valid free block.
        unsafe {
            while cur != NONE && cur < off {
                prev = cur;
                cur = self.header(cur).next_free;
            }
            let mut h = self.header(off);
            h.prev_free = prev;
            h.next_free = cur;
            self.put_header(off, h);
            if prev == NONE {
                self.head.set(off);
            } else {
                self.set_next(prev, off);
            }
            if cur != NONE {
                self.set_prev(cur, off);
            }
        }
    }

    /// Allocates a block, first fit.
    pub fn allocate_block(&self, size: usize, align: usize) -> AllocResult<Block> {
        <Self as Allocator>::allocate(self, size, align)
    }

    /// Structural snapshot computed by walking every block.
    pub fn fragmentation(&self) -> FreeListFragmentation {
        let mut f = FreeListFragmentation::default();
        if self.storage.is_null() {
            return f;
        }
        let mut off = 0usize;
        while off < self.capacity {
            // SAFETY: blocks tile the region, so `off` starts a block while the walk is valid.
            let h = unsafe { self.header(off) };
            if h.magic != BLOCK_MAGIC || h.size < MIN_BLOCK {
                break;
            }
            if h.state == STATE_FREE {
                f.free_bytes += h.size;
                f.free_block_count += 1;
                f.largest_free_block = f.largest_free_block.max(h.size);
            } else {
                f.allocated_block_count += 1;
            }
            off += h.size;
        }
        f
    }

    /// Checks every invariant: physical tiling, footers, no adjacent free blocks,
    /// free-list links and ordering, and list/tile agreement.
    pub fn verify(&self) -> AllocResult<()> {
        if self.storage.is_null() {
            return Ok(());
        }
        let mut off = 0usize;
        let mut free_seen = 0usize;
        let mut prev_free = false;
        while off < self.capacity {
            // SAFETY: the walk follows block sizes; each step checks magic and bounds first.
            let h = unsafe { self.header(off) };
            if h.magic != BLOCK_MAGIC {
                return Err(AllocError::Corruption);
            }
            if h.size < MIN_BLOCK || h.size % GRANULE != 0 || off + h.size > self.capacity {
                return Err(AllocError::Corruption);
            }
            // SAFETY: `off + h.size <= capacity` was checked above.
            if unsafe { self.footer(off + h.size) } != h.size {
                return Err(AllocError::Corruption);
            }
            let is_free = h.state == STATE_FREE;
            if is_free {
                if prev_free {
                    return Err(AllocError::Corruption);
                }
                free_seen += 1;
            }
            prev_free = is_free;
            off += h.size;
        }
        if off != self.capacity {
            return Err(AllocError::Corruption);
        }

        let mut listed = 0usize;
        let mut last = NONE;
        let mut cur = self.head.get();
        while cur != NONE {
            if cur >= self.capacity || cur % GRANULE != 0 {
                return Err(AllocError::Corruption);
            }
            // SAFETY: `cur` was checked to lie inside the region.
            let h = unsafe { self.header(cur) };
            if h.magic != BLOCK_MAGIC || h.state != STATE_FREE {
                return Err(AllocError::Corruption);
            }
            if last != NONE && cur <= last {
                return Err(AllocError::Corruption);
            }
            if h.prev_free != last {
                return Err(AllocError::Corruption);
            }
            last = cur;
            listed += 1;
            if listed > free_seen {
                return Err(AllocError::Corruption);
            }
            cur = h.next_free;
        }
        if listed == free_seen {
            Ok(())
        } else {
            Err(AllocError::Corruption)
        }
    }

    /// Resizes a live block in place.
    ///
    /// Growth absorbs the free block that follows (if it is large enough); shrinking
    /// releases the tail when it is at least 64 bytes. On success the payload address
    /// is unchanged and `block` describes the new size. `CapacityExhausted` means the
    /// block cannot grow in place; the caller may then reallocate and copy.
    ///
    /// # Safety
    /// `block` must be a live block of this heap, with exactly the size it was last
    /// returned with.
    pub unsafe fn try_grow(&self, block: &mut Block, new_size: usize) -> AllocResult<()> {
        if block.is_empty() || block.ptr().is_null() || new_size == 0 {
            return Err(AllocError::InvalidArgument);
        }
        if new_size > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }
        if !self.owns(block.ptr(), block.size()) {
            return Err(AllocError::NotOwned);
        }
        let base = self.storage as usize;
        let payload = block.ptr() as usize;
        if payload - base < HEADER_SIZE + BACK_RECORD {
            return Err(AllocError::InvalidFree);
        }
        // SAFETY: `payload - 16 .. payload` lies inside the region (checked above).
        let (tag, off) = unsafe {
            (
                ptr::read_unaligned(block.ptr().sub(BACK_RECORD) as *const u64),
                ptr::read_unaligned(block.ptr().sub(BACK_RECORD - 8) as *const usize),
            )
        };
        if tag != BACK_TAG ^ off as u64 || off >= self.capacity || off % GRANULE != 0 {
            return Err(AllocError::InvalidFree);
        }
        // SAFETY: `off` was validated against the back-record tag and region bounds.
        let snap = unsafe { self.header(off) };
        if snap.magic != BLOCK_MAGIC || snap.state == STATE_FREE {
            return Err(AllocError::InvalidFree);
        }
        if snap.requested != block.size() {
            return Err(AllocError::InvalidArgument);
        }
        let rel = payload - base - off;
        let need_raw = rel
            .checked_add(new_size)
            .and_then(|v| v.checked_add(FOOTER))
            .ok_or(AllocError::Overflow)?;
        let need = align_up(need_raw, GRANULE).ok_or(AllocError::Overflow)?;
        let old_size = snap.size;
        let mut total = old_size;

        // SAFETY: all block accesses below stay inside the region and follow the
        // invariants that `verify` checks; free-list edits keep the list consistent.
        unsafe {
            if need > old_size {
                let next = off + old_size;
                if next >= self.capacity {
                    return Err(AllocError::CapacityExhausted);
                }
                let nh = self.header(next);
                if nh.magic != BLOCK_MAGIC || nh.state != STATE_FREE || old_size + nh.size < need {
                    return Err(AllocError::CapacityExhausted);
                }
                total = old_size + nh.size;
                self.unlink_free(next);
            }

            let mut alloc_size = total;
            if total >= need && total - need >= MIN_BLOCK {
                alloc_size = need;
                let rem_off = off + need;
                let mut rem = total - need;
                if need <= old_size {
                    // Shrinking: the released tail may merge with a free block after it.
                    let after = off + total;
                    if after < self.capacity {
                        let ah = self.header(after);
                        if ah.magic == BLOCK_MAGIC && ah.state == STATE_FREE {
                            self.unlink_free(after);
                            rem += ah.size;
                        }
                    }
                }
                self.put_header(rem_off, Header {
                    magic: BLOCK_MAGIC,
                    size: rem,
                    requested: 0,
                    next_free: NONE,
                    prev_free: NONE,
                    state: STATE_FREE,
                });
                self.put_footer(rem_off, rem);
                self.insert_free_sorted(rem_off);
            }

            self.put_header(off, Header {
                magic: BLOCK_MAGIC,
                size: alloc_size,
                requested: new_size,
                next_free: NONE,
                prev_free: NONE,
                state: STATE_ALLOCATED,
            });
            self.put_footer(off, alloc_size);
        }
        self.with_stats(|s| s.on_resize(snap.requested, new_size));
        *block = Block::new(block.ptr(), new_size, block.align());
        Ok(())
    }
}

impl Drop for FreeListAllocator {
    fn drop(&mut self) {
        if !self.storage.is_null() {
            // SAFETY: `storage` came from `raw_allocate` in `create` and is released once.
            unsafe { platform::raw_deallocate(self.storage) };
        }
    }
}

impl Allocator for FreeListAllocator {
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
        let base = self.storage as usize;
        let mut off = self.head.get();
        while off != NONE {
            // SAFETY: free-list nodes are valid block starts.
            let snap = unsafe { self.header(off) };
            let block_addr = base + off;
            let payload_addr = match align_up(block_addr + HEADER_SIZE + BACK_RECORD, align) {
                Some(a) => a,
                None => {
                    off = snap.next_free;
                    continue;
                }
            };
            let rel = payload_addr - block_addr;
            let need = match rel.checked_add(size).and_then(|v| v.checked_add(FOOTER)).and_then(|v| align_up(v, GRANULE)) {
                Some(n) => n,
                None => {
                    off = snap.next_free;
                    continue;
                }
            };
            if need > snap.size {
                off = snap.next_free;
                continue;
            }

            let remainder = snap.size - need;
            let take;
            // SAFETY: the block `off` is free and on the list; every write below lies
            // inside it (or in the remainder that replaces it).
            unsafe {
                if remainder >= MIN_BLOCK {
                    let rem_off = off + need;
                    self.put_header(rem_off, Header {
                        magic: BLOCK_MAGIC,
                        size: remainder,
                        requested: 0,
                        next_free: snap.next_free,
                        prev_free: snap.prev_free,
                        state: STATE_FREE,
                    });
                    if snap.prev_free == NONE {
                        self.head.set(rem_off);
                    } else {
                        self.set_next(snap.prev_free, rem_off);
                    }
                    if snap.next_free != NONE {
                        self.set_prev(snap.next_free, rem_off);
                    }
                    self.put_footer(rem_off, remainder);
                    take = need;
                } else {
                    self.unlink_free(off);
                    take = snap.size;
                }
                self.put_header(off, Header {
                    magic: BLOCK_MAGIC,
                    size: take,
                    requested: size,
                    next_free: NONE,
                    prev_free: NONE,
                    state: STATE_ALLOCATED,
                });
                self.put_footer(off, take);
                let p = self.storage.add(payload_addr - base);
                ptr::write_unaligned(p.sub(BACK_RECORD) as *mut u64, BACK_TAG ^ off as u64);
                ptr::write_unaligned(p.sub(BACK_RECORD - 8) as *mut usize, off);
                if DEBUG_CHECKS {
                    ptr::write_bytes(p, UNINIT_BYTE, size);
                }
                self.with_stats(|s| s.on_alloc(size));
                return Ok(Block::new(p, size, align));
            }
        }
        Err(self.fail(AllocError::CapacityExhausted))
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
        let base = self.storage as usize;
        let payload = block.ptr() as usize;
        if payload - base < HEADER_SIZE + BACK_RECORD {
            return Err(AllocError::InvalidFree);
        }
        // SAFETY: `payload - 16 .. payload` lies inside the region (checked above).
        let (tag, off) = unsafe {
            (
                ptr::read_unaligned(block.ptr().sub(BACK_RECORD) as *const u64),
                ptr::read_unaligned(block.ptr().sub(BACK_RECORD - 8) as *const usize),
            )
        };
        if tag != BACK_TAG ^ off as u64 || off >= self.capacity || off % GRANULE != 0 {
            return Err(AllocError::InvalidFree);
        }
        // SAFETY: `off` passed the tag and bounds checks.
        let h = unsafe { self.header(off) };
        if h.magic != BLOCK_MAGIC {
            return Err(AllocError::InvalidFree);
        }
        if h.state == STATE_FREE {
            return Err(AllocError::DoubleFree);
        }
        let rel = payload - base - off;
        if rel < HEADER_SIZE + BACK_RECORD || rel + block.size() + FOOTER > h.size {
            return Err(AllocError::InvalidFree);
        }
        if h.requested != block.size() {
            return Err(AllocError::InvalidArgument);
        }

        let req = h.requested;
        let mut start = off;
        let mut total = h.size;
        // SAFETY: neighbours are validated (magic, state, size) before any edit; the
        // region tiles, so each neighbour offset is a block start.
        unsafe {
            // Merge with the following block if it is free.
            let next_off = off + total;
            if next_off < self.capacity {
                let nh = self.header(next_off);
                if nh.magic == BLOCK_MAGIC && nh.state == STATE_FREE {
                    self.unlink_free(next_off);
                    total += nh.size;
                }
            }
            // Merge with the preceding block if it is free.
            if off >= MIN_BLOCK {
                let prev_size = self.footer(off);
                if prev_size >= MIN_BLOCK && prev_size <= off {
                    let prev_off = off - prev_size;
                    let ph = self.header(prev_off);
                    if ph.magic == BLOCK_MAGIC && ph.state == STATE_FREE && ph.size == prev_size {
                        self.unlink_free(prev_off);
                        start = prev_off;
                        total += prev_size;
                    }
                }
            }
            self.put_header(start, Header {
                magic: BLOCK_MAGIC,
                size: total,
                requested: 0,
                next_free: NONE,
                prev_free: NONE,
                state: STATE_FREE,
            });
            self.put_footer(start, total);
            self.insert_free_sorted(start);
        }
        if DEBUG_CHECKS {
            // SAFETY: the released payload lies inside the region.
            unsafe { ptr::write_bytes(block.ptr(), FREED_BYTE, req) };
        }
        self.with_stats(|s| s.on_free(req));
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE
            .union(Capabilities::INDIVIDUAL_FREE)
            .union(Capabilities::REALLOCATE)
            .union(Capabilities::OWNERSHIP)
            .union(Capabilities::STATISTICS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_validates_and_starts_as_one_block() {
        assert_eq!(FreeListAllocator::create(10).err(), Some(AllocError::InvalidArgument));
        let f = FreeListAllocator::create(1000).unwrap();
        assert_eq!(f.capacity(), 992);
        f.verify().unwrap();
        assert_eq!(f.fragmentation().free_block_count, 1);
    }

    #[test]
    fn coalesces_both_neighbours() {
        let f = FreeListAllocator::create(8192).unwrap();
        let x = f.allocate(200, 16).unwrap();
        let y = f.allocate(200, 16).unwrap();
        let z = f.allocate(200, 16).unwrap();
        assert_eq!(f.fragmentation().allocated_block_count, 3);
        // SAFETY: each block is released exactly once, and checked after every step.
        unsafe {
            f.deallocate(y).unwrap();
            assert_eq!(f.fragmentation().free_block_count, 2);
            f.verify().unwrap();
            f.deallocate(x).unwrap();
            f.verify().unwrap();
            f.deallocate(z).unwrap();
        }
        assert_eq!(f.fragmentation().free_block_count, 1);
        assert_eq!(f.fragmentation().allocated_block_count, 0);
        f.verify().unwrap();
    }

    #[test]
    fn alignment_is_honoured() {
        let f = FreeListAllocator::create(64 * 1024).unwrap();
        for align in [1usize, 16, 64, 256, 4096] {
            let b = f.allocate(100, align).unwrap();
            assert_eq!(b.ptr() as usize & (align - 1), 0);
            // SAFETY: released once.
            unsafe { f.deallocate(b).unwrap() };
        }
        f.verify().unwrap();
    }

    #[test]
    fn misuse_is_refused() {
        let f = FreeListAllocator::create(4096).unwrap();
        let b = f.allocate(64, 16).unwrap();
        let outside = 0u8;
        // SAFETY: each bad call is refused before any unsafe access inside the heap.
        unsafe {
            assert_eq!(
                f.deallocate(Block::from_raw_parts(&outside as *const u8 as *mut u8, 4, 4)),
                Err(AllocError::NotOwned)
            );
            let interior = Block::from_raw_parts(b.ptr().add(8), 8, 16);
            assert_eq!(f.deallocate(interior), Err(AllocError::InvalidFree));
            f.deallocate(b).unwrap();
            assert_eq!(f.deallocate(b), Err(AllocError::DoubleFree));
        }
        f.verify().unwrap();
    }

    #[test]
    fn try_grow_absorbs_free_neighbour() {
        let f = FreeListAllocator::create(8192).unwrap();
        let x = f.allocate(100, 16).unwrap();
        let y = f.allocate(200, 16).unwrap();
        // SAFETY: `y` is released; `x` is then resized (still live, its own size).
        unsafe { f.deallocate(y).unwrap() };
        let mut grown = x;
        // SAFETY: `grown` is live and carries the size it was returned with.
        unsafe { f.try_grow(&mut grown, 300).unwrap() };
        assert_eq!(grown.size(), 300);
        assert_eq!(grown.ptr(), x.ptr());
        assert_eq!(f.stats().bytes_live, 300);
        f.verify().unwrap();
        // SAFETY: released once with its current size.
        unsafe { f.deallocate(grown).unwrap() };
        assert_eq!(f.fragmentation().free_block_count, 1);
        f.verify().unwrap();
    }

    #[test]
    fn try_grow_refuses_when_blocked_and_shrink_releases_tail() {
        let f = FreeListAllocator::create(4096).unwrap();
        let x = f.allocate(64, 16).unwrap();
        let y = f.allocate(64, 16).unwrap();
        let mut blocked = x;
        // SAFETY: `blocked` is live with its current size.
        unsafe {
            assert_eq!(f.try_grow(&mut blocked, 4096), Err(AllocError::CapacityExhausted));
            assert_eq!(blocked.size(), 64);
        }
        // SAFETY: both blocks are live and released once each.
        unsafe {
            f.deallocate(x).unwrap();
            f.deallocate(y).unwrap();
        }
        let big = f.allocate(1000, 16).unwrap();
        let mut shrunk = big;
        // SAFETY: `shrunk` is live with its current size.
        unsafe { f.try_grow(&mut shrunk, 64).unwrap() };
        f.verify().unwrap();
        assert_eq!(f.fragmentation().free_block_count, 1);
        // SAFETY: released once.
        unsafe { f.deallocate(shrunk).unwrap() };
        f.verify().unwrap();
    }

    #[test]
    fn randomized_churn_keeps_invariants_and_no_overlap() {
        let f = FreeListAllocator::create(256 * 1024).unwrap();
        let mut live: Vec<Block> = Vec::new();
        let mut seed: u64 = 0x243F_6A88_85A3_08D3;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for step in 0..3000 {
            if live.is_empty() || rnd() % 5 < 3 {
                let size = 1 + (rnd() % 1500) as usize;
                let align = [1usize, 8, 16, 32, 64, 128][(rnd() % 6) as usize];
                if let Ok(b) = f.allocate(size, align) {
                    for other in &live {
                        let (a0, a1) = (b.ptr() as usize, b.ptr() as usize + b.size());
                        let (c0, c1) = (other.ptr() as usize, other.ptr() as usize + other.size());
                        assert!(a1 <= c0 || c1 <= a0, "overlapping blocks");
                    }
                    live.push(b);
                }
            } else {
                let idx = (rnd() % live.len() as u64) as usize;
                // SAFETY: `live[idx]` is live and released once; it is then removed.
                unsafe { f.deallocate(live[idx]).unwrap() };
                live.swap_remove(idx);
            }
            if step % 97 == 0 {
                f.verify().unwrap();
            }
        }
        for b in live.drain(..) {
            // SAFETY: each remaining block is live and released once.
            unsafe { f.deallocate(b).unwrap() };
        }
        f.verify().unwrap();
        assert_eq!(f.fragmentation().free_block_count, 1);
        assert_eq!(f.stats().live_allocations(), 0);
        assert_eq!(f.stats().accounting_errors, 0);
    }
}

impl crate::allocators::introspect::LiveBlocks for FreeListAllocator {
    fn is_live(&self, block: Block) -> bool {
        if block.is_empty() || block.ptr().is_null() || !self.owns(block.ptr(), block.size()) {
            return false;
        }
        let base = self.storage as usize;
        let payload = block.ptr() as usize;
        if payload - base < HEADER_SIZE + BACK_RECORD {
            return false;
        }
        // SAFETY: `payload - 16 .. payload` lies inside the region (checked above).
        let (tag, off) = unsafe {
            (
                ptr::read_unaligned(block.ptr().sub(BACK_RECORD) as *const u64),
                ptr::read_unaligned(block.ptr().sub(BACK_RECORD - 8) as *const usize),
            )
        };
        if tag != BACK_TAG ^ off as u64 || off >= self.capacity || off % GRANULE != 0 {
            return false;
        }
        // SAFETY: `off` passed the tag and bounds checks.
        let h = unsafe { self.header(off) };
        if h.magic != BLOCK_MAGIC || h.state == STATE_FREE {
            return false;
        }
        let rel = payload - base - off;
        rel >= HEADER_SIZE + BACK_RECORD && rel + block.size() + FOOTER <= h.size && h.requested == block.size()
    }
}
