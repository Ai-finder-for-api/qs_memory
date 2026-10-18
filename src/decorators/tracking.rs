//! Leak tracking decorator: records every live block with its size, alignment and tag.
//!
//! Storage is an open-addressed hash table with linear probing and tombstones, sized at
//! construction (a power of two) and kept at most 3/4 full. When the table is full,
//! `allocate` refuses with `CapacityExhausted` before touching the upstream allocator, so
//! every block handed out is tracked and can be released. Releasing a pointer that is not
//! tracked is refused with `InvalidFree`.
//!
//! Not `Sync`; wrap with `Synchronized` for shared use.

use core::cell::Cell;
use core::mem::{align_of, size_of};
use core::ptr;

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use crate::platform;

const EMPTY: usize = 0;
const TOMBSTONE: usize = 1;
const DEFAULT_TAG: &str = "untagged";

/// One tracked live block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackRecord {
    /// Start address (0 = empty slot, 1 = tombstone).
    pub ptr: usize,
    /// Size requested by the caller.
    pub size: usize,
    /// Alignment requested by the caller.
    pub align: usize,
    /// Tag supplied at allocation time.
    pub tag: Option<&'static str>,
    /// Allocation sequence number.
    pub sequence: u64,
}

/// Live blocks grouped by tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TagSummary {
    /// The tag.
    pub tag: &'static str,
    /// Live blocks carrying the tag.
    pub count: usize,
    /// Their total requested bytes.
    pub bytes: usize,
}

/// Records live allocations for leak reports.
pub struct Tracking<'a, A: Allocator> {
    upstream: &'a A,
    table: *mut TrackRecord,
    slots: usize,
    live: Cell<usize>,
    live_bytes: Cell<usize>,
    peak_bytes: Cell<usize>,
    sequence: Cell<u64>,
    refused: Cell<u64>,
    allocations: Cell<u64>,
    deallocations: Cell<u64>,
}

impl<'a, A: Allocator> Tracking<'a, A> {
    /// Tracks up to about `table_slots` live blocks (rounded to a power of two, minimum 16).
    /// Fails with `OutOfMemory` if the record table cannot be allocated.
    pub fn new(upstream: &'a A, table_slots: usize) -> AllocResult<Self> {
        let mut slots = 16usize;
        while slots < table_slots {
            slots <<= 1;
        }
        let bytes = slots.checked_mul(size_of::<TrackRecord>()).ok_or(AllocError::Overflow)?;
        let table = platform::raw_allocate(bytes, align_of::<TrackRecord>()) as *mut TrackRecord;
        if table.is_null() {
            return Err(AllocError::OutOfMemory);
        }
        // SAFETY: `table` holds `slots` records; an all-zero record is EMPTY (ptr 0, tag None).
        unsafe { ptr::write_bytes(table, 0, slots) };
        Ok(Tracking {
            upstream,
            table,
            slots,
            live: Cell::new(0),
            live_bytes: Cell::new(0),
            peak_bytes: Cell::new(0),
            sequence: Cell::new(0),
            refused: Cell::new(0),
            allocations: Cell::new(0),
            deallocations: Cell::new(0),
        })
    }

    /// Live tracked blocks.
    pub fn live_count(&self) -> usize {
        self.live.get()
    }

    /// Live requested bytes.
    pub fn live_bytes(&self) -> usize {
        self.live_bytes.get()
    }

    /// Largest value `live_bytes` has reached.
    pub fn peak_bytes(&self) -> usize {
        self.peak_bytes.get()
    }

    /// Allocations refused because the table was full.
    pub fn refused(&self) -> u64 {
        self.refused.get()
    }

    /// Allocations served so far.
    pub fn total_allocations(&self) -> u64 {
        self.allocations.get()
    }

    /// Releases served so far.
    pub fn total_deallocations(&self) -> u64 {
        self.deallocations.get()
    }

    fn home(&self, p: usize) -> usize {
        ((p >> 4).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 32) & (self.slots - 1)
    }

    fn insert(&self, rec: TrackRecord) {
        let mut i = self.home(rec.ptr);
        // SAFETY: `i` is always masked into the table.
        unsafe {
            while (*self.table.add(i)).ptr > TOMBSTONE {
                i = (i + 1) & (self.slots - 1);
            }
            *self.table.add(i) = rec;
        }
        self.live.set(self.live.get() + 1);
    }

    fn find(&self, p: usize) -> Option<usize> {
        let mut i = self.home(p);
        for _ in 0..self.slots {
            // SAFETY: `i` is always masked into the table.
            let slot = unsafe { (*self.table.add(i)).ptr };
            if slot == EMPTY {
                return None;
            }
            if slot == p {
                return Some(i);
            }
            i = (i + 1) & (self.slots - 1);
        }
        None
    }

    fn record_at(&self, i: usize) -> TrackRecord {
        // SAFETY: `i` came from `find`, so it is a valid slot index.
        unsafe { *self.table.add(i) }
    }

    /// Allocates a block and records it under `tag`.
    pub fn allocate_tagged(&self, size: usize, align: usize, tag: &'static str) -> AllocResult<Block> {
        if self.live.get() + 1 > (self.slots * 3) / 4 {
            self.refused.set(self.refused.get() + 1);
            return Err(AllocError::CapacityExhausted);
        }
        let b = self.upstream.allocate(size, align)?;
        if b.is_empty() {
            return Ok(b);
        }
        let seq = self.sequence.get() + 1;
        self.sequence.set(seq);
        self.insert(TrackRecord { ptr: b.ptr() as usize, size, align, tag: Some(tag), sequence: seq });
        self.live_bytes.set(self.live_bytes.get() + size);
        if self.live_bytes.get() > self.peak_bytes.get() {
            self.peak_bytes.set(self.live_bytes.get());
        }
        self.allocations.set(self.allocations.get() + 1);
        Ok(b)
    }

    /// Calls `f` for every live tracked block.
    pub fn for_each_live(&self, mut f: impl FnMut(&TrackRecord)) {
        for i in 0..self.slots {
            // SAFETY: `i < slots`.
            let r = unsafe { *self.table.add(i) };
            if r.ptr > TOMBSTONE {
                f(&r);
            }
        }
    }

    /// Groups live blocks by tag into `out`. Returns the number of entries written.
    pub fn summarize(&self, out: &mut [TagSummary]) -> usize {
        let mut used = 0usize;
        self.for_each_live(|r| {
            let tag = r.tag.unwrap_or(DEFAULT_TAG);
            let mut j = 0;
            while j < used && out[j].tag != tag {
                j += 1;
            }
            if j == used {
                if used == out.len() {
                    return;
                }
                out[used] = TagSummary { tag, count: 0, bytes: 0 };
                used += 1;
            }
            out[j].count += 1;
            out[j].bytes += r.size;
        });
        used
    }
}

impl<A: Allocator> Drop for Tracking<'_, A> {
    fn drop(&mut self) {
        if !self.table.is_null() {
            // SAFETY: `table` came from `raw_allocate` in `new` and is released once here.
            unsafe { platform::raw_deallocate(self.table as *mut u8) };
        }
    }
}

impl<A: Allocator> Allocator for Tracking<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        self.allocate_tagged(size, align, DEFAULT_TAG)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        let i = self.find(block.ptr() as usize).ok_or(AllocError::InvalidFree)?;
        let rec = self.record_at(i);
        if rec.size != block.size() {
            return Err(AllocError::InvalidArgument);
        }
        // SAFETY: the block is tracked, so it is live and came from the upstream allocator.
        unsafe { self.upstream.deallocate(block)? };
        // SAFETY: `i` is a valid slot index; mark it as a tombstone.
        unsafe { (*self.table.add(i)).ptr = TOMBSTONE };
        self.live.set(self.live.get() - 1);
        self.live_bytes.set(self.live_bytes.get() - rec.size);
        self.deallocations.set(self.deallocations.get() + 1);
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        self.upstream.capabilities() | Capabilities::STATISTICS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocators::system::SystemAllocator;

    #[test]
    fn counts_live_blocks_and_bytes() {
        let sys = SystemAllocator::new();
        let t = Tracking::new(&sys, 64).unwrap();
        let a = t.allocate_tagged(100, 16, "parser").unwrap();
        let b = t.allocate_tagged(300, 16, "renderer").unwrap();
        assert_eq!((t.live_count(), t.live_bytes(), t.peak_bytes()), (2, 400, 400));
        // SAFETY: each block came from `t` and is released once.
        unsafe {
            t.deallocate(a).unwrap();
            assert_eq!((t.live_count(), t.live_bytes(), t.peak_bytes()), (1, 300, 400));
            t.deallocate(b).unwrap();
        }
        assert_eq!(sys.stats().live_allocations(), 0);
    }

    #[test]
    fn summarizes_leaks_by_tag() {
        let sys = SystemAllocator::new();
        let t = Tracking::new(&sys, 256).unwrap();
        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(t.allocate_tagged(10, 8, "cache").unwrap());
        }
        held.push(t.allocate_tagged(20, 8, "scene").unwrap());
        held.push(t.allocate_tagged(20, 8, "scene").unwrap());
        let mut out = [TagSummary { tag: "", count: 0, bytes: 0 }; 8];
        let n = t.summarize(&mut out);
        assert_eq!(n, 2);
        let cache = out[..n].iter().find(|s| s.tag == "cache").unwrap();
        let scene = out[..n].iter().find(|s| s.tag == "scene").unwrap();
        assert_eq!((cache.count, scene.bytes), (3, 40));
        for b in held {
            // SAFETY: each block came from `t` and is released once.
            unsafe { t.deallocate(b).unwrap() };
        }
        assert_eq!(t.summarize(&mut out), 0);
    }

    #[test]
    fn refuses_unknown_and_mismatched_frees() {
        let sys = SystemAllocator::new();
        let t = Tracking::new(&sys, 64).unwrap();
        let a = t.allocate(64, 16).unwrap();
        let outside = 0u8;
        // SAFETY: bad calls are refused before the upstream sees them; `a` is released once.
        unsafe {
            let wrong = Block::from_raw_parts(a.ptr(), 60, 16);
            assert_eq!(t.deallocate(wrong), Err(AllocError::InvalidArgument));
            assert_eq!(t.live_count(), 1);
            t.deallocate(a).unwrap();
            assert_eq!(t.deallocate(a), Err(AllocError::InvalidFree));
            assert_eq!(
                t.deallocate(Block::from_raw_parts(&outside as *const u8 as *mut u8, 4, 1)),
                Err(AllocError::InvalidFree)
            );
        }
    }

    #[test]
    fn churn_matches_a_model_and_full_table_refuses() {
        let sys = SystemAllocator::new();
        let t = Tracking::new(&sys, 16).unwrap();
        let mut held = Vec::new();
        let mut refused = 0;
        for i in 0..40 {
            match t.allocate_tagged(8, 8, "churn") {
                Ok(b) => held.push(b),
                Err(AllocError::CapacityExhausted) => refused += 1,
                Err(e) => panic!("unexpected {e}"),
            }
            if i % 3 == 2 {
                if let Some(b) = held.pop() {
                    // SAFETY: each block came from `t` and is released once.
                    unsafe { t.deallocate(b).unwrap() };
                }
            }
        }
        assert!(refused > 0);
        assert_eq!(t.refused(), refused as u64);
        assert_eq!(t.live_count(), held.len());
        for b in held {
            // SAFETY: each block came from `t` and is released once.
            unsafe { t.deallocate(b).unwrap() };
        }
        assert_eq!(sys.stats().live_allocations(), 0);
    }
}
