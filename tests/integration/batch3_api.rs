//! Tests for the third feature batch: exact double-free classification, large alignments, the zeroing
//! guard, per-thread caching, sharding, shared memory, large pages, page locking, sampling, timelines,
//! error hooks, settings, the scrubber, JSON snapshots, memory queries, and fragmentation maps.

use core::sync::atomic::{AtomicUsize, Ordering};
use qs_memory::{
    clear_error_hook, set_error_hook, system_memory, AllocError, AllocResult, Allocator, Block, Capabilities,
    HugeArena, LiveBlocks, Located, Ownership, PageKind, PageLocked, Reporting, RingAllocator, Sampled, Scrubber,
    Settings, Sharded, SharedArena, SystemAllocator, ThreadCached, Timeline, Tlsf, Zeroing,
};
use std::cell::Cell;
use std::sync::Arc;
use std::time::Duration;

fn unique(tag: &str) -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    format!("qs_{tag}_{}_{nanos}", std::process::id())
}

#[test]
fn tlsf_double_free_after_merge_is_reported_exactly() {
    let heap = Tlsf::new(4096).unwrap();
    let a = heap.allocate(64, 16).unwrap();
    let b = heap.allocate(64, 16).unwrap();
    let c = heap.allocate(64, 16).unwrap();
    // SAFETY: `a` and `b` are live and released once; `b` is merged into `a` by this release.
    unsafe {
        heap.deallocate(a).unwrap();
        heap.deallocate(b).unwrap();
    }
    // SAFETY: deliberate double release of the absorbed block; the heap refuses it without corrupting itself.
    assert_eq!(unsafe { heap.deallocate(b) }, Err(AllocError::DoubleFree));
    heap.verify().unwrap();
    // SAFETY: `c` is live and released once.
    unsafe { heap.deallocate(c).unwrap() };
    heap.verify().unwrap();
    assert_eq!(heap.free_block_count(), 1);
}

#[test]
fn tlsf_refuses_interior_pointers_without_touching_the_heap() {
    let heap = Tlsf::new(4096).unwrap();
    let a = heap.allocate(64, 16).unwrap();
    // SAFETY: the interior block is never read; the heap refuses it because it is not a block start.
    let interior = unsafe { Block::from_raw_parts(a.ptr().add(16), 16, 16) };
    // SAFETY: deliberate misuse; refused before any memory is touched.
    assert_eq!(unsafe { heap.deallocate(interior) }, Err(AllocError::InvalidFree));
    heap.verify().unwrap();
    // SAFETY: `a` is live and released once.
    unsafe { heap.deallocate(a).unwrap() };
    heap.verify().unwrap();
}

#[test]
fn tlsf_honours_large_alignments() {
    let heap = Tlsf::new(1 << 16).unwrap();
    let mut blocks = Vec::new();
    for align in [32usize, 64, 256, 1024, 4096] {
        let block = heap.allocate(100, align).unwrap();
        assert_eq!(block.ptr() as usize % align, 0, "alignment {align}");
        assert!(block.size() >= 100);
        heap.verify().unwrap();
        blocks.push(block);
    }
    for block in blocks.drain(..) {
        // SAFETY: each block is live and released once.
        unsafe { heap.deallocate(block).unwrap() };
        heap.verify().unwrap();
    }
    assert_eq!(heap.free_block_count(), 1);
}

#[test]
fn tlsf_renders_a_fragmentation_map_and_usage() {
    let heap = Tlsf::new(1024).unwrap();
    let a = heap.allocate(256, 16).unwrap();
    let map = heap.render_map(16);
    assert_eq!(map.len(), 16);
    assert!(map.contains('#') && map.contains('.'));
    let usage = heap.usage();
    assert_eq!(usage.used_blocks, 1);
    assert!(usage.largest_free > 0);
    // SAFETY: `a` is live and released once.
    unsafe { heap.deallocate(a).unwrap() };
    assert!(!heap.render_map(16).contains('#'));
}

/// Reads the block in `deallocate` (it is still live there) and records whether every byte is zero.
struct Probe<'a> {
    heap: &'a Tlsf,
    zero_at_release: Cell<bool>,
}

impl Allocator for Probe<'_> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        self.heap.allocate(size, align)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        // SAFETY: the block is still live here; the decorator forwards it before anything is released.
        let bytes = unsafe { core::slice::from_raw_parts(block.ptr(), block.size()) };
        self.zero_at_release.set(bytes.iter().all(|&b| b == 0));
        // SAFETY: the caller guarantees the block is live and from this allocator.
        unsafe { self.heap.deallocate(block) }
    }

    fn capabilities(&self) -> Capabilities {
        self.heap.capabilities()
    }
}

impl LiveBlocks for Probe<'_> {
    fn is_live(&self, block: Block) -> bool {
        self.heap.is_live(block)
    }
}

#[test]
fn zeroing_wipes_live_blocks_and_writes_nothing_on_misuse() {
    let heap = Tlsf::new(4096).unwrap();
    let probe = Probe { heap: &heap, zero_at_release: Cell::new(false) };
    let zeroing = Zeroing::new(&probe);

    let block = zeroing.allocate(48, 8).unwrap();
    // SAFETY: the block is live and 48 bytes long.
    unsafe { block.as_mut_slice().fill(0xAB) };
    // SAFETY: the block is live and released once.
    unsafe { zeroing.deallocate(block).unwrap() };
    assert!(probe.zero_at_release.get());

    let live = zeroing.allocate(64, 16).unwrap();
    // SAFETY: the block is live and 64 bytes long.
    unsafe { live.as_mut_slice().fill(0x5A) };
    // SAFETY: the interior block is never written; `Zeroing` refuses it because it is not live.
    let interior = unsafe { Block::from_raw_parts(live.ptr().add(16), 16, 16) };
    // SAFETY: deliberate misuse; the decorator must not write to it.
    assert_eq!(unsafe { zeroing.deallocate(interior) }, Err(AllocError::InvalidFree));
    // SAFETY: the block is live and 64 bytes long.
    assert!(unsafe { live.as_mut_slice() }.iter().all(|&b| b == 0x5A));
    // SAFETY: the block is live and released once.
    unsafe { zeroing.deallocate(live).unwrap() };
}

#[test]
fn ring_reports_liveness_of_its_blocks() {
    let ring = RingAllocator::new(256).unwrap();
    let a = ring.allocate(32, 16).unwrap();
    assert!(ring.is_live(a));
    // SAFETY: `a` is live and released once.
    unsafe { ring.deallocate(a).unwrap() };
    assert!(!ring.is_live(a));
}

#[test]
fn thread_cache_reuses_blocks_of_its_size_class() {
    let cache = ThreadCached::new(SystemAllocator::new(), 64, 16, 4);
    let a = cache.allocate(64, 16).unwrap();
    // SAFETY: `a` is live and released once; the cache keeps it for reuse.
    unsafe { cache.deallocate(a).unwrap() };
    assert_eq!(cache.cached_blocks(), 4);
    let b = cache.allocate(64, 16).unwrap();
    assert_eq!(b.ptr(), a.ptr());
    let other = cache.allocate(10, 8).unwrap();
    // SAFETY: both blocks are live and released once; `other` bypasses the cache.
    unsafe {
        cache.deallocate(other).unwrap();
        cache.deallocate(b).unwrap();
    }
    cache.flush().unwrap();
    assert_eq!(cache.cached_blocks(), 0);
}

#[test]
fn sharded_allocator_routes_each_release_to_its_owner() {
    let shards = Sharded::new(vec![Tlsf::new(2048).unwrap(), Tlsf::new(2048).unwrap()]);
    assert_eq!(shards.shard_count(), 2);
    let mut blocks = Vec::new();
    for _ in 0..40 {
        if let Ok(block) = shards.allocate(64, 16) {
            blocks.push(block);
        }
    }
    assert!(blocks.len() > 20);
    for block in blocks.drain(..) {
        // SAFETY: each block is live and released once, through the set that issued it.
        unsafe { shards.deallocate(block).unwrap() };
    }
    let local = 0u8;
    // SAFETY: a stack byte owned by no shard; it is refused without being read.
    let stray = unsafe { Block::from_raw_parts(&local as *const u8 as *mut u8, 16, 16) };
    // SAFETY: deliberate misuse; refused.
    assert_eq!(unsafe { shards.deallocate(stray) }, Err(AllocError::NotOwned));
}

#[test]
fn shared_arena_is_visible_to_a_second_handle() {
    let name = unique("arena");
    let a = SharedArena::create(&name, 4096).unwrap();
    let b = SharedArena::open(&name, 4096).unwrap();
    let x = a.allocate(100, 16).unwrap();
    let y = b.allocate(100, 16).unwrap();
    assert_ne!(x.ptr(), y.ptr());
    assert!(a.used() >= 200);
    assert_eq!(a.used(), b.used());
    // SAFETY: `x` is a block of 100 bytes reserved for this test.
    unsafe { x.as_mut_slice().fill(1) };
    assert!(a.owns(x.ptr(), 100));
    let local = 0u8;
    assert!(!a.owns(&local as *const u8, 1));
    // SAFETY: deliberate misuse; a shared arena does not free single blocks.
    assert_eq!(unsafe { a.deallocate(x) }, Err(AllocError::NotSupported));
    assert_eq!(SharedArena::open(&name, 1024).err(), Some(AllocError::InvalidArgument));
}

#[test]
fn huge_arena_reports_the_pages_it_obtained() {
    let arena = HugeArena::new(1 << 20).unwrap();
    assert!(matches!(arena.page_kind(), PageKind::Large | PageKind::Normal));
    let block = arena.allocate(1000, 256).unwrap();
    assert_eq!(block.ptr() as usize % 256, 0);
    assert!(arena.capacity() >= 1 << 20);
    assert!(arena.used() >= 1000);
    // SAFETY: the block is 1000 bytes long and owned by the arena.
    unsafe { block.as_mut_slice().fill(7) };
}

#[test]
fn page_locked_blocks_are_returned_even_when_locking_is_refused() {
    let sys = SystemAllocator::new();
    let locked = PageLocked::new(&sys);
    let block = locked.allocate(4096, 16).unwrap();
    // SAFETY: the block is 4096 bytes long and owned here.
    unsafe { block.as_mut_slice().fill(3) };
    assert!(locked.lock_failures() <= 1);
    // SAFETY: the block is live and released once.
    unsafe { locked.deallocate(block).unwrap() };
    assert_eq!(sys.stats().live_allocations(), 0);
}

#[test]
fn sampler_records_every_nth_allocation_and_scales_the_estimate() {
    let sys = SystemAllocator::new();
    let sampled = Sampled::new(&sys, 3);
    let mut blocks = Vec::new();
    for _ in 0..9 {
        blocks.push(sampled.allocate(10, 8).unwrap());
    }
    assert_eq!(sampled.allocations(), 9);
    let samples = sampled.samples();
    assert_eq!(samples.len(), 3);
    assert!(samples.iter().all(|s| s.file.ends_with("batch3_api.rs")));
    assert_eq!(sampled.estimated_bytes(), 90);
    for block in blocks {
        // SAFETY: each block is live and released once.
        unsafe { sampled.deallocate(block).unwrap() };
    }
}

#[test]
fn timeline_exports_csv_and_the_peak() {
    let sys = SystemAllocator::new();
    let timeline = Timeline::new(&sys, 100);
    let a = timeline.allocate(100, 8).unwrap();
    let b = timeline.allocate(300, 8).unwrap();
    // SAFETY: each block is live and released once.
    unsafe {
        timeline.deallocate(a).unwrap();
        timeline.deallocate(b).unwrap();
    }
    assert_eq!(timeline.events().len(), 4);
    assert_eq!(timeline.peak_bytes(), 400);
    let csv = timeline.to_csv();
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines[0], "micros,op,bytes");
    assert_eq!(lines.len(), 5);
    assert!(lines[1].ends_with(",alloc,100"));
}

static HOOKED: AtomicUsize = AtomicUsize::new(0);

fn count_error(_e: AllocError) {
    HOOKED.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn reporting_decorator_feeds_the_error_hook() {
    let heap = Tlsf::new(4096).unwrap();
    let reporting = Reporting::new(&heap);
    set_error_hook(count_error);
    let before = HOOKED.load(Ordering::SeqCst);
    assert!(reporting.allocate(1 << 20, 16).is_err());
    assert!(HOOKED.load(Ordering::SeqCst) > before);
    clear_error_hook();
}

#[test]
fn settings_parse_the_environment_with_defaults() {
    assert_eq!(Settings::parse(|_| None), Settings::default());
    let s = Settings::parse(|k| match k {
        "QS_MEMORY_QUARANTINE" => Some("64".to_string()),
        "QS_MEMORY_POISON" => Some("TRUE".to_string()),
        _ => None,
    });
    assert_eq!(s.quarantine_blocks, 64);
    assert!(s.poison_on_free);
    let bad = Settings::parse(|k| if k == "QS_MEMORY_QUARANTINE" { Some("lots".to_string()) } else { None });
    assert_eq!(bad.quarantine_blocks, 0);
}

#[test]
fn scrubber_runs_checks_until_stopped() {
    let runs = Arc::new(AtomicUsize::new(0));
    let counter = runs.clone();
    let scrubber = Scrubber::spawn(Duration::from_millis(5), move || {
        counter.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    std::thread::sleep(Duration::from_millis(120));
    assert!(scrubber.passes() >= 1);
    assert_eq!(scrubber.failures(), 0);
    assert_eq!(scrubber.last_error(), None);
    scrubber.stop();
    let after = runs.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(runs.load(Ordering::SeqCst), after);
}

#[test]
fn located_snapshot_is_json_with_call_sites() {
    let sys = SystemAllocator::new();
    let loc = Located::new(&sys);
    let block = loc.allocate(100, 8).unwrap();
    let json = loc.snapshot_json();
    assert!(json.starts_with('[') && json.ends_with(']'));
    assert!(json.contains("\"bytes\":100"));
    assert!(json.contains("batch3_api.rs"));
    // SAFETY: the block is live and released once.
    unsafe { loc.deallocate(block).unwrap() };
    assert_eq!(loc.snapshot_json(), "[]");
}

#[test]
fn system_memory_reports_plausible_numbers() {
    let info = system_memory().expect("the OS answers memory queries");
    assert!(info.total_physical > 0);
    assert!(info.available_physical <= info.total_physical);
    assert!(info.page_size.is_power_of_two());
}
