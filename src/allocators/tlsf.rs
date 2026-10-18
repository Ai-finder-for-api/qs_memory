//! Two-level segregated fit (TLSF) allocator.
//!
//! The region is a sequence of blocks. Each block starts with a 16-byte header:
//!
//! - word 0: the block size, with a 4-bit tag in the low bits (`ALLOC_TAG` or `FREE_TAG`);
//! - word 1: the offset of the physically previous block, or `NONE` for the first block.
//!
//! Free blocks keep their free-list links in their payload. Free lists are indexed by a first level
//! (the power of two of the size) and a second level (one of 16 slices of that power), so finding a
//! fitting block is two bit scans.
//!
//! Misuse detection is exact for the cases below, and it never reads a header that is not a block
//! start:
//!
//! - A bitmap records where blocks start. A pointer that is not a block start is refused with
//!   `InvalidFree`, which covers interior pointers and pointers into merged free space.
//! - A second bitmap records blocks that were freed and then absorbed into a neighbour. Releasing
//!   such a block again is reported as `DoubleFree`.
//! - A block that is still free and still a block start is reported as `DoubleFree`.
//!
//! The one case the bitmaps cannot classify is a pointer whose block was freed, absorbed, and then
//! its memory handed out again inside a larger allocation. Such a pointer is no longer a block start
//! or a freed block, so it is reported as `InvalidFree`. The heap is never corrupted either way.
//!
//! Alignment can be any power of two. Larger alignments carve a free front block so the payload lands
//! on an aligned address.

use crate::allocators::introspect::LiveBlocks;
use crate::allocators::system::SystemAllocator;
use crate::allocators::traits::Allocator;
use crate::decorators::Ownership;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use core::cell::Cell;

const HDR: usize = 16;
const UNIT: usize = 16;
const MIN_BLOCK: usize = 32;
const NONE: usize = usize::MAX;
const TAG_MASK: usize = 0xF;
const ALLOC_TAG: usize = 0xA;
const FREE_TAG: usize = 0x5;
const SMALL: usize = 256;
const SL: usize = 16;
const FL: usize = 64;

fn floor_log2(n: usize) -> usize {
    (usize::BITS - 1 - n.leading_zeros()) as usize
}

/// Free-list indices for a block of `size` bytes (exact placement).
fn mapping_insert(size: usize) -> (usize, usize) {
    if size < SMALL {
        (0, size >> 4)
    } else {
        let fl = floor_log2(size);
        (fl - 7, (size >> (fl - 4)) & (SL - 1))
    }
}

/// Free-list indices from which any block can satisfy a request of `size` bytes.
fn mapping_search(size: usize) -> (usize, usize) {
    if size >= SMALL {
        let round = (1usize << (floor_log2(size) - 4)) - 1;
        mapping_insert(size + round)
    } else {
        mapping_insert(size)
    }
}

/// One bit per 16-byte unit of the region.
struct Bits {
    words: Vec<Cell<u64>>,
}

impl Bits {
    fn new(units: usize) -> Self {
        Bits { words: (0..units.div_ceil(64)).map(|_| Cell::new(0)).collect() }
    }
    fn get(&self, i: usize) -> bool {
        self.words[i / 64].get() & (1u64 << (i % 64)) != 0
    }
    fn set(&self, i: usize, on: bool) {
        let w = &self.words[i / 64];
        let mask = 1u64 << (i % 64);
        w.set(if on { w.get() | mask } else { w.get() & !mask });
    }
    fn count(&self) -> usize {
        self.words.iter().map(|w| w.get().count_ones() as usize).sum()
    }
}

/// Usage counters for a TLSF heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TlsfUsage {
    pub free_blocks: usize,
    pub used_blocks: usize,
    pub free_bytes: usize,
    pub used_bytes: usize,
    pub largest_free: usize,
}

/// A TLSF heap over a region it owns.
pub struct Tlsf {
    backing: SystemAllocator,
    region: Block,
    base: *mut u8,
    len: usize,
    fl_map: Cell<u64>,
    sl_map: Vec<Cell<u16>>,
    heads: Vec<Cell<usize>>,
    starts: Bits,
    hints: Bits,
    zero_on_free: Cell<bool>,
}

impl Tlsf {
    /// Creates a heap over `capacity` bytes (rounded down to 16). The region starts as one free block.
    pub fn new(capacity: usize) -> AllocResult<Self> {
        let len = capacity & !(UNIT - 1);
        if len < MIN_BLOCK {
            return Err(AllocError::InvalidArgument);
        }
        let backing = SystemAllocator::new();
        let region = backing.allocate(len, UNIT)?;
        let heap = Tlsf {
            backing,
            region,
            base: region.ptr(),
            len,
            fl_map: Cell::new(0),
            sl_map: (0..FL).map(|_| Cell::new(0)).collect(),
            heads: (0..FL * SL).map(|_| Cell::new(NONE)).collect(),
            starts: Bits::new(len / UNIT),
            hints: Bits::new(len / UNIT),
            zero_on_free: Cell::new(false),
        };
        // SAFETY: offset 0 is the start of the region, which holds at least MIN_BLOCK bytes.
        unsafe { heap.write_header(0, len, FREE_TAG, NONE) };
        heap.mark_start(0);
        // SAFETY: block 0 was just written as a free block of `len` bytes.
        unsafe { heap.insert_free(0, len) };
        Ok(heap)
    }

    /// Size of the region in bytes.
    pub fn capacity(&self) -> usize {
        self.len
    }

    /// Block sizes, used and free, for every block in address order: `(offset, size, free)`.
    fn blocks(&self) -> Vec<(usize, usize, bool)> {
        let mut out = Vec::new();
        let mut off = 0;
        while off < self.len {
            // SAFETY: `off` walks block starts inside the region.
            let (size, tag, _) = unsafe { self.read(off) };
            out.push((off, size, tag == FREE_TAG));
            if size == 0 {
                break;
            }
            off += size;
        }
        out
    }

    /// Usage counters: block counts, byte totals, and the largest free block.
    pub fn usage(&self) -> TlsfUsage {
        let mut u = TlsfUsage { free_blocks: 0, used_blocks: 0, free_bytes: 0, used_bytes: 0, largest_free: 0 };
        for (_, size, free) in self.blocks() {
            if free {
                u.free_blocks += 1;
                u.free_bytes += size - HDR;
                u.largest_free = u.largest_free.max(size - HDR);
            } else {
                u.used_blocks += 1;
                u.used_bytes += size - HDR;
            }
        }
        u
    }

    /// Number of free blocks, found by walking the region.
    pub fn free_block_count(&self) -> usize {
        self.usage().free_blocks
    }

    /// A one-line-per-row map of the region: `#` for used space and `.` for free space, `width` columns
    /// wide. Each column stands for an equal slice of the region.
    pub fn render_map(&self, width: usize) -> String {
        let width = width.max(1);
        let blocks = self.blocks();
        let mut out = String::with_capacity(width);
        for i in 0..width {
            let pos = i * self.len / width;
            let free = blocks
                .iter()
                .find(|(off, size, _)| pos >= *off && pos < off + size)
                .map_or(true, |(_, _, free)| *free);
            out.push(if free { '.' } else { '#' });
        }
        out
    }

    unsafe fn word(&self, off: usize, index: usize) -> *mut usize {
        // SAFETY (caller): `off` is the start of a block inside the region and `index` is within a block.
        unsafe { (self.base.add(off) as *mut usize).add(index) }
    }

    unsafe fn read(&self, off: usize) -> (usize, usize, usize) {
        // SAFETY: `off` is a 16-byte aligned block start inside the region, so the header is readable.
        unsafe {
            let w0 = *self.word(off, 0);
            let prev = *self.word(off, 1);
            (w0 & !TAG_MASK, w0 & TAG_MASK, prev)
        }
    }

    unsafe fn write_header(&self, off: usize, size: usize, tag: usize, prev: usize) {
        // SAFETY: `off` is a block start inside the region.
        unsafe {
            *self.word(off, 0) = size | tag;
            *self.word(off, 1) = prev;
        }
    }

    fn mark_start(&self, off: usize) {
        self.starts.set(off / UNIT, true);
        self.hints.set(off / UNIT, false);
    }

    /// Records that the block starting at `off` was freed and then absorbed into a neighbour.
    fn absorb(&self, off: usize) {
        self.starts.set(off / UNIT, false);
        self.hints.set(off / UNIT, true);
    }

    unsafe fn insert_free(&self, off: usize, size: usize) {
        let (fl, sl) = mapping_insert(size);
        let idx = fl * SL + sl;
        let head = self.heads[idx].get();
        // SAFETY: a free block has at least MIN_BLOCK bytes, so words 2 and 3 are inside it.
        unsafe {
            *self.word(off, 2) = head;
            *self.word(off, 3) = NONE;
            if head != NONE {
                *self.word(head, 3) = off;
            }
        }
        self.heads[idx].set(off);
        self.sl_map[fl].set(self.sl_map[fl].get() | (1u16 << sl));
        self.fl_map.set(self.fl_map.get() | (1u64 << fl));
    }

    unsafe fn remove_free(&self, off: usize, size: usize) {
        let (fl, sl) = mapping_insert(size);
        let idx = fl * SL + sl;
        // SAFETY: `off` is a free block of `size` bytes on its list, so its links are valid.
        let (next, prev) = unsafe { (*self.word(off, 2), *self.word(off, 3)) };
        if prev != NONE {
            // SAFETY: `prev` is a free block on the same list.
            unsafe { *self.word(prev, 2) = next };
        } else {
            self.heads[idx].set(next);
        }
        if next != NONE {
            // SAFETY: `next` is a free block on the same list.
            unsafe { *self.word(next, 3) = prev };
        }
        if self.heads[idx].get() == NONE {
            let bits = self.sl_map[fl].get() & !(1u16 << sl);
            self.sl_map[fl].set(bits);
            if bits == 0 {
                self.fl_map.set(self.fl_map.get() & !(1u64 << fl));
            }
        }
    }

    fn find_suitable(&self, fl: usize, sl: usize) -> Option<(usize, usize)> {
        let same_level = self.sl_map[fl].get() & (!0u16 << sl);
        if same_level != 0 {
            return Some((fl, same_level.trailing_zeros() as usize));
        }
        let higher = self.fl_map.get() & (!0u64).checked_shl(fl as u32 + 1).unwrap_or(0);
        if higher == 0 {
            return None;
        }
        let f = higher.trailing_zeros() as usize;
        Some((f, self.sl_map[f].get().trailing_zeros() as usize))
    }

    /// Turns the free block at `off` (of `bsize` bytes, previously removed from its list) into an allocated
    /// block of `need` bytes, returning the rest as a free block when it is large enough. Returns the
    /// allocated size.
    unsafe fn carve(&self, off: usize, bsize: usize, need: usize, prev: usize) -> usize {
        let end = off + bsize;
        let (final_size, last_start) = if bsize - need >= MIN_BLOCK {
            let rest = off + need;
            let rest_size = bsize - need;
            // SAFETY: `rest` lies inside the block being split, which has `bsize` bytes.
            unsafe { self.write_header(rest, rest_size, FREE_TAG, off) };
            self.mark_start(rest);
            // SAFETY: `rest` is a free block of `rest_size` bytes.
            unsafe { self.insert_free(rest, rest_size) };
            (need, rest)
        } else {
            (bsize, off)
        };
        // SAFETY: `off` is a block start inside the region.
        // Memory handed out again is no longer "freed": clear any freed marks inside it, so pointers into it
        // are classified as invalid rather than as double frees.
        for unit in (off / UNIT)..((off + final_size) / UNIT) {
            self.hints.set(unit, false);
        }
        unsafe { self.write_header(off, final_size, ALLOC_TAG, prev) };
        self.mark_start(off);
        if end < self.len {
            // SAFETY: `end` is the next block start; its previous block is now `last_start`.
            unsafe { *self.word(end, 1) = last_start };
        }
        final_size
    }

    /// Checks every structural invariant: blocks tile the region, tags are valid, physical links agree, no
    /// two free blocks are adjacent, the start bitmap matches the blocks, no start is marked as freed, and
    /// the free lists and bitmaps match the free blocks.
    pub fn verify(&self) -> AllocResult<()> {
        let mut off = 0;
        let mut prev = NONE;
        let mut prev_free = false;
        let mut free_blocks = 0usize;
        let mut used_blocks = 0usize;
        while off < self.len {
            // SAFETY: `off` walks block starts inside the region.
            let (size, tag, p) = unsafe { self.read(off) };
            let valid_tag = tag == ALLOC_TAG || tag == FREE_TAG;
            if !valid_tag || size < MIN_BLOCK || size % UNIT != 0 || off + size > self.len || p != prev {
                return Err(AllocError::Corruption);
            }
            if !self.starts.get(off / UNIT) || self.hints.get(off / UNIT) {
                return Err(AllocError::Corruption);
            }
            let free = tag == FREE_TAG;
            if free {
                if prev_free {
                    return Err(AllocError::Corruption);
                }
                free_blocks += 1;
            } else {
                used_blocks += 1;
            }
            prev_free = free;
            prev = off;
            off += size;
        }
        if off != self.len || self.starts.count() != free_blocks + used_blocks {
            return Err(AllocError::Corruption);
        }
        let mut listed = 0usize;
        for fl in 0..FL {
            let bits = self.sl_map[fl].get();
            if (bits != 0) != (self.fl_map.get() & (1u64 << fl) != 0) {
                return Err(AllocError::Corruption);
            }
            for sl in 0..SL {
                let idx = fl * SL + sl;
                let mut node = self.heads[idx].get();
                if (node != NONE) != (bits & (1u16 << sl) != 0) {
                    return Err(AllocError::Corruption);
                }
                while node != NONE {
                    // SAFETY: `node` is a block start on a free list.
                    let (size, tag, _) = unsafe { self.read(node) };
                    if tag != FREE_TAG || mapping_insert(size) != (fl, sl) {
                        return Err(AllocError::Corruption);
                    }
                    listed += 1;
                    if listed > free_blocks {
                        return Err(AllocError::Corruption);
                    }
                    // SAFETY: as above.
                    node = unsafe { *self.word(node, 2) };
                }
            }
        }
        if listed != free_blocks {
            return Err(AllocError::Corruption);
        }
        Ok(())
    }

    /// Resolves a block pointer to its offset, or the error to report for it.
    fn offset_of(&self, addr: usize) -> Result<usize, AllocError> {
        let start = self.base as usize;
        if addr < start + HDR || addr >= start + self.len {
            return Err(AllocError::NotOwned);
        }
        let off = addr - start - HDR;
        if off % UNIT != 0 {
            return Err(AllocError::InvalidFree);
        }
        Ok(off)
    }
}

impl Allocator for Tlsf {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !align.is_power_of_two() {
            return Err(AllocError::InvalidAlignment);
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        let payload = size.checked_add(UNIT - 1).ok_or(AllocError::Overflow)? & !(UNIT - 1);
        let need = payload.checked_add(HDR).ok_or(AllocError::Overflow)?.max(MIN_BLOCK);
        let extra = if align > UNIT { align + MIN_BLOCK } else { 0 };
        let search = need.checked_add(extra).ok_or(AllocError::Overflow)?;
        if search > self.len {
            return Err(AllocError::CapacityExhausted);
        }
        let (fl, sl) = mapping_search(search);
        let (f, s) = self.find_suitable(fl, sl).ok_or(AllocError::CapacityExhausted)?;
        let off = self.heads[f * SL + s].get();
        // SAFETY: `off` is the head of a non-empty free list, so it is a valid free block start.
        let (bsize, _, prev) = unsafe { self.read(off) };
        // SAFETY: `off` is on the list for its size.
        unsafe { self.remove_free(off, bsize) };

        let (mut block_off, mut block_size, mut block_prev) = (off, bsize, prev);
        if align > UNIT {
            let addr = self.base as usize + off + HDR;
            if addr % align != 0 {
                let aligned = (addr + MIN_BLOCK).next_multiple_of(align);
                let gap = aligned - addr;
                // SAFETY: the front gap lies inside the block being carved; it is at least MIN_BLOCK bytes.
                unsafe { self.write_header(off, gap, FREE_TAG, prev) };
                self.mark_start(off);
                // SAFETY: the gap is a free block of `gap` bytes.
                unsafe { self.insert_free(off, gap) };
                block_off = off + gap;
                block_size = bsize - gap;
                block_prev = off;
            }
        }
        // SAFETY: `block_off` is a free block of `block_size` bytes, removed from its list above.
        let final_size = unsafe { self.carve(block_off, block_size, need, block_prev) };
        // SAFETY: the allocated block holds `final_size` bytes starting at `block_off`, inside the region.
        let ptr = unsafe { self.base.add(block_off + HDR) };
        // SAFETY: the payload is `final_size - HDR` bytes inside the region, aligned to `align`.
        Ok(unsafe { Block::from_raw_parts(ptr, final_size - HDR, align) })
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return Ok(());
        }
        let off = self.offset_of(block.ptr() as usize)?;
        if !self.starts.get(off / UNIT) {
            return Err(if self.hints.get(off / UNIT) { AllocError::DoubleFree } else { AllocError::InvalidFree });
        }
        // SAFETY: `off` is a block start inside the region, so its header is readable.
        let (size, tag, prev) = unsafe { self.read(off) };
        if tag == FREE_TAG {
            return Err(AllocError::DoubleFree);
        }
        if tag != ALLOC_TAG || size < MIN_BLOCK || off + size > self.len {
            return Err(AllocError::InvalidFree);
        }
        if self.zero_on_free.get() {
            for i in 0..(size - HDR) {
                // SAFETY: the block was validated above, so the payload bytes after its header are writable.
                unsafe { core::ptr::write_volatile(block.ptr().add(i), 0) };
            }
        }
        let mut start_off = off;
        let mut total = size;
        let mut new_prev = prev;
        if prev != NONE {
            // SAFETY: `prev` is the physical previous block, which is a valid block start.
            let (psize, ptag, pprev) = unsafe { self.read(prev) };
            if ptag == FREE_TAG {
                // SAFETY: the previous block is free and on the list for its size.
                unsafe { self.remove_free(prev, psize) };
                self.absorb(off);
                start_off = prev;
                total += psize;
                new_prev = pprev;
            }
        }
        let after = off + size;
        if after < self.len {
            // SAFETY: `after` is the next block start.
            let (nsize, ntag, _) = unsafe { self.read(after) };
            if ntag == FREE_TAG {
                // SAFETY: the next block is free and on the list for its size.
                unsafe { self.remove_free(after, nsize) };
                self.absorb(after);
                total += nsize;
            }
        }
        // SAFETY: `start_off` begins a block of `total` bytes that is now free.
        unsafe { self.write_header(start_off, total, FREE_TAG, new_prev) };
        self.mark_start(start_off);
        let end = start_off + total;
        if end < self.len {
            // SAFETY: `end` is the next block start; its previous block is now `start_off`.
            unsafe { *self.word(end, 1) = start_off };
        }
        // SAFETY: the merged block is free and has `total` bytes.
        unsafe { self.insert_free(start_off, total) };
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::ALLOCATE.union(Capabilities::INDIVIDUAL_FREE).union(Capabilities::OWNERSHIP)
    }
}

impl LiveBlocks for Tlsf {
    fn is_live(&self, block: Block) -> bool {
        if block.is_empty() {
            return false;
        }
        let Ok(off) = self.offset_of(block.ptr() as usize) else {
            return false;
        };
        if !self.starts.get(off / UNIT) {
            return false;
        }
        // SAFETY: `off` is a block start inside the region.
        let (size, tag, _) = unsafe { self.read(off) };
        tag == ALLOC_TAG && block.size() <= size - HDR
    }
}

impl Ownership for Tlsf {
    fn owns(&self, p: *const u8, n: usize) -> bool {
        let start = self.base as usize;
        let addr = p as usize;
        addr >= start && addr.checked_add(n).map_or(false, |end| end <= start + self.len)
    }
}

impl Drop for Tlsf {
    fn drop(&mut self) {
        // SAFETY: `region` came from `backing` and is released exactly once, here.
        let _ = unsafe { self.backing.deallocate(self.region) };
    }
}

// SAFETY: a `Tlsf` exclusively owns its region; the raw base pointer refers only to memory that this value
// allocated and releases. Moving the whole heap to another thread therefore moves that ownership with it.
// It is not `Sync`, so it is never shared between threads without a lock.
unsafe impl Send for Tlsf {}

impl Tlsf {
    /// When on, every released block is overwritten with zeros before it is merged or reused.
    pub fn set_zero_on_free(&self, on: bool) {
        self.zero_on_free.set(on);
    }

    /// Whether zero-on-free is on.
    pub fn zero_on_free(&self) -> bool {
        self.zero_on_free.get()
    }
}
