//! Tests for the first feature batch: resize, typed helpers, bulk operations, call-site capture,
//! quarantine and poisoning, peak tracking, and leak checks.

use core::mem::align_of;
use qs_memory::decorators::quarantine::POISON;
use qs_memory::{
    AllocError, Allocator, AllocatorExt, BulkAllocator, LeakCheck, Located, PoolAllocator, Quarantine,
    Resize, SystemAllocator, Watermark,
};

#[test]
fn grow_keeps_the_original_bytes() {
    let sys = SystemAllocator::new();
    let block = sys.allocate(16, 8).unwrap();
    // SAFETY: the block is live and 16 bytes long.
    unsafe { block.as_mut_slice().copy_from_slice(&[7u8; 16]) };
    // SAFETY: `block` is live and from `sys`; it is not used after the grow.
    let grown = unsafe { sys.grow(block, 64).unwrap() };
    assert_eq!(grown.size(), 64);
    // SAFETY: `grown` is live and 64 bytes long.
    let bytes = unsafe { grown.as_mut_slice() };
    assert!(bytes[..16].iter().all(|&b| b == 7));
    // SAFETY: released once.
    unsafe { sys.deallocate(grown).unwrap() };
}

#[test]
fn grow_refuses_a_smaller_size() {
    let sys = SystemAllocator::new();
    let block = sys.allocate(32, 8).unwrap();
    // SAFETY: the refused call leaves `block` untouched, so it is still released below.
    assert_eq!(unsafe { sys.grow(block, 8) }.unwrap_err(), AllocError::InvalidArgument);
    // SAFETY: released once.
    unsafe { sys.deallocate(block).unwrap() };
}

#[test]
fn shrink_keeps_the_prefix_and_zero_releases() {
    let sys = SystemAllocator::new();
    let block = sys.allocate(32, 8).unwrap();
    // SAFETY: the block is live and 32 bytes long.
    unsafe { block.as_mut_slice().fill(3) };
    // SAFETY: `block` is live and from `sys`; it is not used after the shrink.
    let small = unsafe { sys.shrink(block, 8).unwrap() };
    assert_eq!(small.size(), 8);
    // SAFETY: `small` is live and 8 bytes long.
    assert!(unsafe { small.as_mut_slice() }.iter().all(|&b| b == 3));
    // SAFETY: `small` is live; shrinking to zero releases it.
    let empty = unsafe { sys.shrink(small, 0).unwrap() };
    assert!(empty.is_empty());
    assert_eq!(sys.stats().live_allocations(), 0);
}

#[test]
fn realloc_handles_every_case() {
    let sys = SystemAllocator::new();
    // An empty block is allocated fresh.
    // SAFETY: the empty block needs no release.
    let fresh = unsafe { sys.realloc(qs_memory::Block::empty(8), 24).unwrap() };
    assert_eq!(fresh.size(), 24);
    // SAFETY: `fresh` is live and from `sys`.
    let grown = unsafe { sys.realloc(fresh, 100).unwrap() };
    assert!(grown.size() >= 100);
    // SAFETY: `grown` is live and from `sys`; a smaller request keeps the block.
    let same = unsafe { sys.realloc(grown, 50).unwrap() };
    assert_eq!(same.ptr(), grown.ptr());
    // SAFETY: released once.
    assert!(unsafe { sys.realloc(same, 0).unwrap() }.is_empty());
    assert_eq!(sys.stats().live_allocations(), 0);
}

#[test]
fn typed_helpers_use_the_type_layout() {
    let sys = SystemAllocator::new();
    let block = sys.allocate_for::<u64>().unwrap();
    assert_eq!(block.size(), 8);
    assert_eq!(block.ptr() as usize % align_of::<u64>(), 0);
    // SAFETY: released once.
    unsafe { sys.deallocate(block).unwrap() };
    assert_eq!(sys.allocate_array::<u32>(usize::MAX).unwrap_err(), AllocError::Overflow);
    let copy = sys.allocate_copy_of(&[1u32, 2, 3]).unwrap();
    // SAFETY: `copy` holds three initialised u32 values, and it is released below.
    let words = unsafe { core::slice::from_raw_parts(copy.ptr() as *const u32, 3) };
    assert_eq!(words, &[1, 2, 3]);
    // SAFETY: released once.
    unsafe { sys.deallocate(copy).unwrap() };
}

#[test]
fn allocate_many_is_all_or_nothing() {
    let pool = PoolAllocator::create(64, 16, 3).unwrap();
    assert!(pool.allocate_many(&[32, 32, 32, 32], 16).is_err());
    assert_eq!(pool.slots_in_use(), 0);
    let mut blocks = pool.allocate_many(&[8, 8, 8], 16).unwrap();
    assert_eq!(blocks.len(), 3);
    // SAFETY: each block is live, from `pool`, and listed once.
    unsafe { pool.deallocate_many(&mut blocks).unwrap() };
    assert!(blocks.is_empty());
    assert_eq!(pool.slots_in_use(), 0);
}

#[test]
fn located_groups_live_blocks_by_call_site() {
    let sys = SystemAllocator::new();
    let loc = Located::new(&sys);
    let a = loc.allocate(100, 8).unwrap();
    let b = loc.allocate(100, 8).unwrap();
    let c = loc.allocate(40, 8).unwrap();
    let sites = loc.sites();
    assert_eq!(sites.len(), 3);
    assert_eq!(loc.live_bytes(), 240);
    assert!(sites.iter().all(|s| s.file.ends_with("features_api.rs")));
    // SAFETY: each block is live, from `loc`, and released once.
    unsafe {
        loc.deallocate(a).unwrap();
        loc.deallocate(b).unwrap();
        loc.deallocate(c).unwrap();
    }
    assert_eq!(loc.live_count(), 0);
    assert_eq!(sys.stats().live_allocations(), 0);
}

#[test]
fn quarantine_detects_a_write_after_free() {
    let sys = SystemAllocator::new();
    let q = Quarantine::new(&sys, 4);
    let block = q.allocate(32, 8).unwrap();
    // SAFETY: the block is live and released once; the quarantine keeps its memory valid.
    unsafe { q.deallocate(block).unwrap() };
    assert_eq!(q.check(), Ok(()));
    // SAFETY: deliberate double free; refused before any access.
    assert_eq!(unsafe { q.deallocate(block) }, Err(AllocError::DoubleFree));
    // A write into freed memory, which the check must catch.
    // SAFETY: the quarantined memory is still allocated, so the write is in bounds.
    unsafe { *block.ptr() = 0x41 };
    assert_eq!(q.check(), Err(AllocError::Corruption));
    // Repair the poison so the flush releases the block cleanly.
    // SAFETY: as above.
    unsafe { *block.ptr() = POISON };
    assert_eq!(q.check(), Ok(()));
    q.flush().unwrap();
    assert_eq!(sys.stats().live_allocations(), 0);
}

#[test]
fn quarantine_releases_the_oldest_block_past_its_limit() {
    let sys = SystemAllocator::new();
    let q = Quarantine::new(&sys, 2);
    let blocks: Vec<_> = (0..3).map(|_| q.allocate(16, 8).unwrap()).collect();
    for b in &blocks {
        // SAFETY: each block is live and released once.
        unsafe { q.deallocate(*b).unwrap() };
    }
    assert_eq!(q.held(), 2);
    // Quarantined blocks stay allocated upstream; only the evicted one was released.
    assert_eq!(q.held(), 2);
    assert_eq!(sys.stats().live_allocations(), 2);
    q.flush().unwrap();
    assert_eq!(sys.stats().live_allocations(), 0);
}

#[test]
fn watermark_keeps_peaks_until_reset() {
    let sys = SystemAllocator::new();
    let w = Watermark::new(&sys);
    let a = w.allocate(100, 8).unwrap();
    let b = w.allocate(300, 8).unwrap();
    // SAFETY: released once.
    unsafe { w.deallocate(a).unwrap() };
    let p = w.peaks();
    assert_eq!(p.live_bytes, 300);
    assert_eq!(p.peak_bytes, 400);
    assert_eq!(p.peak_blocks, 2);
    w.reset_peak();
    assert_eq!(w.peaks().peak_bytes, 300);
    // SAFETY: released once.
    unsafe { w.deallocate(b).unwrap() };
}

#[test]
fn leak_check_counts_outstanding_blocks() {
    let sys = SystemAllocator::new();
    let leak = LeakCheck::new("features", &sys);
    let block = leak.allocate(64, 8).unwrap();
    assert!(!leak.is_clean());
    assert_eq!(leak.live_blocks(), 1);
    assert!(leak.leak_message().unwrap().contains("64"));
    // SAFETY: released once.
    unsafe { leak.deallocate(block).unwrap() };
    assert!(leak.is_clean());
    assert!(leak.leak_message().is_none());
}
