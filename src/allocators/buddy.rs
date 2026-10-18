//! Binary buddy allocator over one power-of-two region.
//!
//! - Blocks are power-of-two sized, from 64 bytes up to the region size.
//! - A block of size S starts at an offset that is a multiple of S, so its address is
//!   aligned to min(S, base alignment). A request is served from the smallest block with
//!   S >= max(size, alignment), which satisfies the alignment.
//! - A side table holds one byte per 64-byte unit: 0 when no block starts there,
//!   otherwise `(order + 1)` with a free flag. Double frees and invalid frees are
//!   detected from this table in O(1).
//! - Free blocks of each order form a doubly-linked list stored inside the blocks.
//! - Freeing merges a block with its buddy while the buddy is free at the same order.
//!
//! Trade-off: internal fragmentation, because requests round up to a power of two.
//! Not `Sync`.

use core::cell::Cell;
use core::mem::size_of;
use core::ptr;

use crate::foundation::align::is_valid_alignment;
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::{MAX_ALIGNMENT, MAX_BLOCK_SIZE};
use crate::foundation::error::{AllocError, AllocResult};
use crate::foundation::stats::Stats;
use crate::platform;

const UNIT: usize = 64;
const MAX_ORDERS: usize = 48;
const FREE_FLAG: u8 = 0x80;
const NONE: usize = usize::MAX;

#[repr(C)]
#[derive(Clone, Copy)]
struct Node {
    next: usize,
    prev: usize,
}

const _: () = assert!(size_of::<Node>() <= UNIT, "free-block node must fit the smallest block");

/// Structural snapshot of a buddy heap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuddyStats {
    /// Bytes in free blocks.
    pub free_bytes: usize,
    /// Largest free block.
    pub largest_free_block: usize,
    /// Number of free blocks.
    pub free_blocks: usize,
    /// Number of allocated blocks.
    pub allocated_blocks: usize,
    /// Highest order in use (the whole region).
    pub max_order: usize,
}

/// Power-of-two heap with O(1) validation.
pub struct BuddyAllocator {
    storage: *mut u8,
    table: *mut u8,
    capacity: usize,
    units: usize,
    max_order: usize,
    heads: [Cell<usize>; MAX_ORDERS],
    stats: Cell<Stats>,
}

// SAFETY: the heap owns its region and table and has no thread affinity; it is not `Sync`.
unsafe impl Send for BuddyAllocator {}

fn block_size(order: usize) -> usize {
    UNIT << order
}

fn order_for(bytes: usize) -> usize {
    let mut order = 0;
    let mut block = UNIT;
    while block < bytes {
        block <<= 1;
        order += 1;
    }
    order
}

impl BuddyAllocator {
    /// Creates a heap over the largest power of two not exceeding `capacity`.
    /// Requires at least 64 bytes.
    pub fn create(capacity: usize) -> AllocResult<Self> {
        if capacity < UNIT {
            return Err(AllocError::InvalidArgument);
        }
        let mut cap = UNIT;
        while cap <= capacity / 2 {
            cap <<= 1;
        }
        if cap > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }
        let storage = platform::raw_allocate(cap, cap.min(MAX_ALIGNMENT));
        if storage.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        let units = cap / UNIT;
        let table = platform::raw_allocate(units, 1);
        if table.is_null() {
            // SAFETY: `storage` came from `raw_allocate` and is released once here.
            unsafe { platform::raw_deallocate(storage) };
            return Err(AllocError::OutOfMemory);
        }
        // SAFETY: `table` holds `units` bytes.
        unsafe { ptr::write_bytes(table, 0, units) };
        let mut top = 0;
        while block_size(top) != cap {
            top += 1;
        }
        let heap = BuddyAllocator {
            storage,
            table,
            capacity: cap,
            units,
            max_order: top,
            heads: core::array::from_fn(|_| Cell::new(NONE)),
            stats: Cell::new(Stats { capacity_bytes: cap as u64, ..Stats::default() }),
        };
        heap.push_free(0, top);
        Ok(heap)
    }

    /// Heap size in bytes (a power of two).
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
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

    fn with_stats(&self, f: impl FnOnce(&mut Stats)) {
        let mut s = self.stats.get();
        f(&mut s);
        self.stats.set(s);
    }

    fn fail(&self, e: AllocError) -> AllocError {
        self.with_stats(|s| s.on_failure());
        e
    }

    fn entry(&self, off: usize) -> u8 {
        // SAFETY: `off < capacity`, so `off / UNIT < units`.
        unsafe { *self.table.add(off / UNIT) }
    }

    fn set_entry(&self, off: usize, v: u8) {
        // SAFETY: `off < capacity`, so `off / UNIT < units`.
        unsafe { *self.table.add(off / UNIT) = v };
    }

    fn node(&self, off: usize) -> Node {
        // SAFETY: free-list offsets always name free blocks, which hold a node.
        unsafe { ptr::read_unaligned(self.storage.add(off) as *const Node) }
    }

    fn set_node(&self, off: usize, n: Node) {
        // SAFETY: as in `node`.
        unsafe { ptr::write_unaligned(self.storage.add(off) as *mut Node, n) };
    }

    fn push_free(&self, off: usize, order: usize) {
        let head = self.heads[order].get();
        self.set_node(off, Node { next: head, prev: NONE });
        if head != NONE {
            let mut h = self.node(head);
            h.prev = off;
            self.set_node(head, h);
        }
        self.heads[order].set(off);
        self.set_entry(off, (order as u8 + 1) | FREE_FLAG);
    }

    fn remove_free(&self, off: usize, order: usize) {
        let n = self.node(off);
        if n.prev == NONE {
            self.heads[order].set(n.next);
        } else {
            let mut p = self.node(n.prev);
            p.next = n.next;
            self.set_node(n.prev, p);
        }
        if n.next != NONE {
            let mut q = self.node(n.next);
            q.prev = n.prev;
            self.set_node(n.next, q);
        }
    }

    /// Structural snapshot computed from the side table.
    pub fn structure(&self) -> BuddyStats {
        let mut s = BuddyStats { max_order: self.max_order, ..BuddyStats::default() };
        if self.table.is_null() {
            return s;
        }
        let mut i = 0usize;
        while i < self.units {
            let e = self.entry(i * UNIT);
            if e == 0 {
                break;
            }
            let order = (e & !FREE_FLAG) as usize - 1;
            let bs = block_size(order);
            if e & FREE_FLAG != 0 {
                s.free_blocks += 1;
                s.free_bytes += bs;
                s.largest_free_block = s.largest_free_block.max(bs);
            } else {
                s.allocated_blocks += 1;
            }
            i += bs / UNIT;
        }
        s
    }

    /// Checks tiling, free-list links, and buddy merging. Returns `Corruption` on failure.
    pub fn verify(&self) -> AllocResult<()> {
        if self.table.is_null() {
            return Ok(());
        }
        let mut covered = 0usize;
        let mut free_count = 0usize;
        let mut i = 0usize;
        while i < self.units {
            let e = self.entry(i * UNIT);
            if e == 0 {
                return Err(AllocError::Corruption);
            }
            let order = (e & !FREE_FLAG) as usize - 1;
            if order > self.max_order {
                return Err(AllocError::Corruption);
            }
            let bs = block_size(order);
            let off = i * UNIT;
            if off % bs != 0 || i + bs / UNIT > self.units {
                return Err(AllocError::Corruption);
            }
            if e & FREE_FLAG != 0 {
                free_count += 1;
                let buddy = off ^ bs;
                if buddy + bs <= self.capacity && self.entry(buddy) == (order as u8 + 1) | FREE_FLAG {
                    return Err(AllocError::Corruption);
                }
            }
            covered += bs;
            i += bs / UNIT;
        }
        if covered != self.capacity {
            return Err(AllocError::Corruption);
        }

        let mut listed = 0usize;
        for order in 0..=self.max_order {
            let mut prev = NONE;
            let mut cur = self.heads[order].get();
            while cur != NONE {
                if self.entry(cur) != (order as u8 + 1) | FREE_FLAG {
                    return Err(AllocError::Corruption);
                }
                if self.node(cur).prev != prev {
                    return Err(AllocError::Corruption);
                }
                prev = cur;
                listed += 1;
                if listed > free_count {
                    return Err(AllocError::Corruption);
                }
                cur = self.node(cur).next;
            }
        }
        if listed == free_count {
            Ok(())
        } else {
            Err(AllocError::Corruption)
        }
    }
}

impl Drop for BuddyAllocator {
    fn drop(&mut self) {
        if !self.storage.is_null() {
            // SAFETY: `storage` came from `raw_allocate` in `create` and is released once.
            unsafe { platform::raw_deallocate(self.storage) };
        }
        if !self.table.is_null() {
            // SAFETY: `table` came from `raw_allocate` in `create` and is released once.
            unsafe { platform::raw_deallocate(self.table) };
        }
    }
}

impl Allocator for BuddyAllocator {
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
        if self.storage.is_null() {
            return Err(self.fail(AllocError::CapacityExhausted));
        }
        let order = order_for(size.max(align));
        if order > self.max_order {
            return Err(self.fail(AllocError::CapacityExhausted));
        }
        let mut k = order;
        while k <= self.max_order && self.heads[k].get() == NONE {
            k += 1;
        }
        if k > self.max_order {
            return Err(self.fail(AllocError::CapacityExhausted));
        }
        let off = self.heads[k].get();
        self.remove_free(off, k);
        self.set_entry(off, 0);
        // Split down to the requested order, freeing the upper buddy at each step.
        while k > order {
            k -= 1;
            self.push_free(off + block_size(k), k);
        }
        self.set_entry(off, order as u8 + 1);
        self.with_stats(|s| s.on_alloc(size));
        // SAFETY: `off < capacity`, so the block lies inside the storage.
        let p = unsafe { self.storage.add(off) };
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
        let off = block.ptr() as usize - self.storage as usize;
        if off % UNIT != 0 {
            return Err(AllocError::InvalidFree);
        }
        let e = self.entry(off);
        if e == 0 {
            return Err(AllocError::InvalidFree);
        }
        if e & FREE_FLAG != 0 {
            return Err(AllocError::DoubleFree);
        }
        let mut order = e as usize - 1;
        let bs = block_size(order);
        if off % bs != 0 || block.size() > bs {
            return Err(AllocError::InvalidArgument);
        }
        self.with_stats(|s| s.on_free(block.size()));
        self.set_entry(off, 0);

        // Coalesce upward while the buddy is free at the same order.
        let mut cur = off;
        while order < self.max_order {
            let size_k = block_size(order);
            let buddy = cur ^ size_k;
            if buddy + size_k > self.capacity {
                break;
            }
            if self.entry(buddy) != (order as u8 + 1) | FREE_FLAG {
                break;
            }
            self.remove_free(buddy, order);
            self.set_entry(buddy, 0);
            cur = cur.min(buddy);
            order += 1;
        }
        self.push_free(cur, order);
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE
            .union(Capabilities::INDIVIDUAL_FREE)
            .union(Capabilities::STATISTICS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_down_to_power_of_two() {
        assert_eq!(BuddyAllocator::create(10).err(), Some(AllocError::InvalidArgument));
        let b = BuddyAllocator::create(1000).unwrap();
        assert_eq!(b.capacity(), 512);
        b.verify().unwrap();
        assert_eq!(b.structure().largest_free_block, 512);
    }

    #[test]
    fn splits_and_merges_back_to_whole() {
        let b = BuddyAllocator::create(4096).unwrap();
        let x = b.allocate(64, 1).unwrap();
        let y = b.allocate(64, 1).unwrap();
        b.verify().unwrap();
        assert_eq!(b.structure().allocated_blocks, 2);
        // SAFETY: each block is released once.
        unsafe {
            b.deallocate(x).unwrap();
            b.deallocate(y).unwrap();
        }
        b.verify().unwrap();
        assert_eq!(b.structure().free_blocks, 1);
        assert_eq!(b.structure().largest_free_block, 4096);
    }

    #[test]
    fn alignment_up_to_block_size() {
        let b = BuddyAllocator::create(64 * 1024).unwrap();
        for align in [1usize, 64, 256, 4096] {
            let blk = b.allocate(10, align).unwrap();
            assert_eq!(blk.ptr() as usize & (align - 1), 0);
            // SAFETY: released once.
            unsafe { b.deallocate(blk).unwrap() };
        }
        b.verify().unwrap();
    }

    #[test]
    fn exhaustion_and_recovery() {
        let b = BuddyAllocator::create(1024).unwrap();
        assert_eq!(b.allocate(2048, 1).unwrap_err(), AllocError::CapacityExhausted);
        let full = b.allocate(1024, 1).unwrap();
        assert_eq!(b.allocate(64, 1).unwrap_err(), AllocError::CapacityExhausted);
        // SAFETY: released once.
        unsafe { b.deallocate(full).unwrap() };
        assert!(b.allocate(64, 1).is_ok());
    }

    #[test]
    fn double_free_and_invalid_pointers() {
        let b = BuddyAllocator::create(4096).unwrap();
        let x = b.allocate(100, 1).unwrap();
        let y = b.allocate(100, 1).unwrap();
        let outside = 0u8;
        // SAFETY: each misuse is refused before any access; each valid block is released once.
        unsafe {
            b.deallocate(x).unwrap();
            assert_eq!(b.deallocate(x), Err(AllocError::DoubleFree));
            let misaligned = Block::from_raw_parts(y.ptr().add(3), 100, 1);
            assert_eq!(b.deallocate(misaligned), Err(AllocError::InvalidFree));
            assert_eq!(
                b.deallocate(Block::from_raw_parts(&outside as *const u8 as *mut u8, 4, 1)),
                Err(AllocError::NotOwned)
            );
            b.deallocate(y).unwrap();
        }
        b.verify().unwrap();
    }

    #[test]
    fn randomized_invariants() {
        let b = BuddyAllocator::create(1 << 18).unwrap();
        let mut live: Vec<Block> = Vec::new();
        let mut seed: u64 = 0xDEAD_BEEF_CAFE_F00D;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for step in 0..4000 {
            if live.is_empty() || rnd() % 5 < 3 {
                let size = 1 + (rnd() % 3000) as usize;
                let align = 1usize << (rnd() % 7);
                if let Ok(blk) = b.allocate(size, align) {
                    assert_eq!(blk.ptr() as usize & (align - 1), 0);
                    live.push(blk);
                }
            } else {
                let idx = (rnd() % live.len() as u64) as usize;
                // SAFETY: `live[idx]` is live and released once; it is then removed.
                unsafe { b.deallocate(live[idx]).unwrap() };
                live.swap_remove(idx);
            }
            if step % 61 == 0 {
                b.verify().unwrap();
            }
        }
        for blk in live.drain(..) {
            // SAFETY: each remaining block is live and released once.
            unsafe { b.deallocate(blk).unwrap() };
        }
        b.verify().unwrap();
        assert_eq!(b.structure().free_blocks, 1);
        assert_eq!(b.stats().live_allocations(), 0);
    }
}

impl crate::allocators::introspect::LiveBlocks for BuddyAllocator {
    fn is_live(&self, block: Block) -> bool {
        if block.is_empty() || block.ptr().is_null() || !self.owns(block.ptr(), block.size()) {
            return false;
        }
        let off = block.ptr() as usize - self.storage as usize;
        if off % UNIT != 0 {
            return false;
        }
        let e = self.entry(off);
        if e == 0 || e & FREE_FLAG != 0 {
            return false;
        }
        let bs = block_size(e as usize - 1);
        off % bs == 0 && block.size() <= bs
    }
}
