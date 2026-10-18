//! Tests for the fourth feature batch: containers, lock-free structures under contention, read-write
//! locking, epoch reclamation, small-object caching, bulk memory helpers, checksums, zero-on-free for TLSF
//! and the pool, and the cross-slot double-release check.

use qs_memory::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[test]
fn small_string_stays_inline_until_it_must_spill() {
    let sys = SystemAllocator::new();
    let mut s: SmallString<'_, 8, SystemAllocator> = SmallString::new(&sys);
    s.push_str("hello").unwrap();
    assert!(s.is_inline());
    s.push(' ').unwrap();
    s.push_str("world, long enough to spill").unwrap();
    assert!(!s.is_inline());
    assert_eq!(s.as_str(), "hello world, long enough to spill");
    s.clear();
    assert!(s.is_empty());
    assert_eq!(s.as_str(), "");
}

#[test]
fn deque_grows_on_both_ends_and_keeps_order() {
    let sys = SystemAllocator::new();
    let mut d: Deque<'_, u32, SystemAllocator> = Deque::new(&sys).unwrap();
    for i in 0..10 {
        d.push_back(i).unwrap();
    }
    for i in 0..5 {
        d.push_front(100 + i).unwrap();
    }
    assert_eq!(d.len(), 15);
    assert_eq!(d.front(), Some(&104));
    assert_eq!(d.back(), Some(&9));
    let order: Vec<u32> = d.iter().copied().collect();
    assert_eq!(order, vec![104, 103, 102, 101, 100, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert_eq!(d.pop_front(), Some(104));
    assert_eq!(d.pop_back(), Some(9));
    assert_eq!(d.get(0), Some(&103));
}

#[test]
fn priority_queue_pops_in_descending_order() {
    let sys = SystemAllocator::new();
    let mut q: PriorityQueue<'_, i64, SystemAllocator> = PriorityQueue::new(&sys).unwrap();
    for v in [5, -3, 17, 0, 9, 9, 2] {
        q.push(v).unwrap();
    }
    assert_eq!(q.peek(), Some(&17));
    let mut out = Vec::new();
    while let Some(v) = q.pop() {
        out.push(v);
    }
    assert_eq!(out, vec![17, 9, 9, 5, 2, 0, -3]);
}

#[test]
fn ordered_map_keeps_keys_sorted() {
    let sys = SystemAllocator::new();
    let mut m: OrderedMap<'_, u32, &str, SystemAllocator> = OrderedMap::new(&sys).unwrap();
    assert_eq!(m.insert(30, "c").unwrap(), None);
    m.insert(10, "a").unwrap();
    m.insert(20, "b").unwrap();
    assert_eq!(m.insert(20, "B").unwrap(), Some("b"));
    let keys: Vec<u32> = m.iter().map(|(k, _)| *k).collect();
    assert_eq!(keys, vec![10, 20, 30]);
    assert_eq!(m.get(&20), Some(&"B"));
    assert_eq!(m.remove(&10), Some("a"));
    assert_eq!(m.first(), Some((&20, &"B")));
    assert_eq!(m.last(), Some((&30, &"c")));
    assert!(!m.contains_key(&10));
}

#[test]
fn hash_set_tracks_membership() {
    let sys = SystemAllocator::new();
    let mut s: HashSet<'_, u64, SystemAllocator> = HashSet::new(&sys).unwrap();
    assert!(s.insert(7).unwrap());
    assert!(!s.insert(7).unwrap());
    assert!(s.contains(&7));
    assert!(s.remove(&7));
    assert!(!s.remove(&7));
    assert!(s.is_empty());
}

#[test]
fn lru_evicts_the_least_recently_used_entry() {
    let sys = SystemAllocator::new();
    let mut c: LruCache<'_, &str, u32, SystemAllocator> = LruCache::new(&sys, 2).unwrap();
    assert_eq!(c.put("a", 1).unwrap(), None);
    assert_eq!(c.put("b", 2).unwrap(), None);
    assert_eq!(c.get(&"a"), Some(&1));
    let evicted = c.put("c", 3).unwrap();
    assert_eq!(evicted, Some(("b", 2)));
    assert!(c.contains(&"a") && c.contains(&"c") && !c.contains(&"b"));
    assert_eq!(c.peek(&"c"), Some(&3));
    assert_eq!(c.len(), 2);
    assert!(LruCache::<&str, u32, SystemAllocator>::new(&sys, 0).is_err());
}

#[test]
fn bitset_grows_and_counts() {
    let sys = SystemAllocator::new();
    let mut b = BitSet::new(&sys).unwrap();
    b.set(3).unwrap();
    b.set(200).unwrap();
    assert!(b.get(3) && b.get(200) && !b.get(4));
    assert_eq!(b.count(), 2);
    assert_eq!(b.iter().collect::<Vec<_>>(), vec![3, 200]);
    b.clear(3);
    assert_eq!(b.count(), 1);
}

#[test]
fn byte_builder_and_cursor_round_trip() {
    let sys = SystemAllocator::new();
    let mut w = ByteBuilder::new(&sys).unwrap();
    w.put_u8(0xAB).unwrap();
    w.put_u16_le(0x1234).unwrap();
    w.put_u32_le(0xDEAD_BEEF).unwrap();
    w.put_u64_le(0x0102_0304_0506_0708).unwrap();
    w.put_bytes(b"tail").unwrap();
    assert_eq!(w.len(), 1 + 2 + 4 + 8 + 4);
    let mut r = Cursor::new(w.as_slice());
    assert_eq!(r.read_u8(), Ok(0xAB));
    assert_eq!(r.read_u16_le(), Ok(0x1234));
    assert_eq!(r.read_u32_le(), Ok(0xDEAD_BEEF));
    assert_eq!(r.read_u64_le(), Ok(0x0102_0304_0506_0708));
    assert_eq!(r.read_bytes(4), Ok(&b"tail"[..]));
    assert_eq!(r.remaining(), 0);
    assert_eq!(r.read_u8(), Err(ReadError::OutOfData));
}

#[test]
fn lock_free_stack_keeps_every_value_under_contention() {
    let stack = Arc::new(LockFreeStack::new(20_000));
    let threads = 4u64;
    let per_thread = 5_000u64;
    let mut handles = Vec::new();
    for t in 0..threads {
        let s = stack.clone();
        handles.push(std::thread::spawn(move || {
            let mut pushed_sum = 0u64;
            for i in 0..per_thread {
                let v = t * per_thread + i + 1;
                loop {
                    if s.push(v).is_ok() {
                        break;
                    }
                    std::thread::yield_now();
                }
                pushed_sum += v;
            }
            pushed_sum
        }));
    }
    let pushed: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();
    let mut popped_sum = 0u64;
    let mut popped = 0u64;
    while let Some(v) = stack.pop() {
        popped_sum += v;
        popped += 1;
    }
    assert_eq!(popped, threads * per_thread);
    assert_eq!(popped_sum, pushed);
}

#[test]
fn bounded_queue_delivers_every_value_once() {
    let queue = Arc::new(BoundedQueue::new(64));
    let producers = 3u64;
    let per_producer = 4_000u64;
    let total = producers * per_producer;
    let received = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for p in 0..producers {
        let q = queue.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..per_producer {
                let mut v = p * per_producer + i;
                loop {
                    match q.push(v) {
                        Ok(()) => break,
                        Err(back) => {
                            v = back;
                            std::thread::yield_now();
                        }
                    }
                }
            }
        }));
    }
    let mut consumers = Vec::new();
    for _ in 0..2 {
        let q = queue.clone();
        let r = received.clone();
        consumers.push(std::thread::spawn(move || {
            let mut sum = 0u64;
            while r.load(Ordering::SeqCst) < total as usize {
                if let Some(v) = q.pop() {
                    sum += v;
                    r.fetch_add(1, Ordering::SeqCst);
                } else {
                    std::thread::yield_now();
                }
            }
            sum
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let consumed: u64 = consumers.into_iter().map(|h| h.join().unwrap()).sum();
    assert_eq!(received.load(Ordering::SeqCst) as u64, total);
    assert_eq!(consumed, (0..total).sum::<u64>());
    assert!(queue.pop().is_none());
}

#[test]
fn rw_locked_allows_readers_alongside_each_other_and_serialises_writers() {
    let locked = Arc::new(RwLocked::new(SystemAllocator::new()));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let l = locked.clone();
        handles.push(std::thread::spawn(move || {
            for _ in 0..200 {
                let b = l.allocate(32, 8).unwrap();
                // SAFETY: released once, through the same wrapper that issued it.
                unsafe { l.deallocate(b).unwrap() };
                let _ = l.read(|a| a.stats().live_allocations());
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(locked.read(|a| a.stats().live_allocations()), 0);
}

#[test]
fn epoch_defers_reclamation_until_the_pinned_reader_leaves() {
    let epoch = Epoch::new(2);
    let freed = Arc::new(AtomicUsize::new(0));
    let reader = epoch.pin().expect("a participant is free");
    let f = freed.clone();
    epoch.retire(move || {
        f.fetch_add(1, Ordering::SeqCst);
    });
    for _ in 0..4 {
        epoch.collect();
    }
    assert_eq!(freed.load(Ordering::SeqCst), 0, "a pinned reader must keep retired memory alive");
    drop(reader);
    for _ in 0..4 {
        epoch.collect();
    }
    assert_eq!(freed.load(Ordering::SeqCst), 1);
    assert_eq!(epoch.pending(), 0);
}

#[test]
fn epoch_refuses_pins_beyond_its_participants() {
    let epoch = Epoch::new(1);
    let first = epoch.pin();
    assert!(first.is_some());
    assert!(epoch.pin().is_none());
    drop(first);
    assert!(epoch.pin().is_some());
}

#[test]
fn small_cache_reuses_its_class_and_refuses_double_release() {
    let sys = SystemAllocator::new();
    let cache = SmallCache::new(&sys, 48, 16, 4);
    let a = cache.allocate(48, 16).unwrap();
    // SAFETY: released once; the cache keeps it for reuse.
    unsafe { cache.deallocate(a).unwrap() };
    assert_eq!(cache.cached(), 1);
    // SAFETY: deliberate double release while cached; refused.
    assert_eq!(unsafe { cache.deallocate(a) }, Err(AllocError::DoubleFree));
    let b = cache.allocate(48, 16).unwrap();
    assert_eq!(b.ptr(), a.ptr());
    assert_eq!(cache.hits(), 1);
    // SAFETY: released once.
    unsafe { cache.deallocate(b).unwrap() };
    cache.flush().unwrap();
    assert_eq!(sys.stats().live_allocations(), 0);
}

#[test]
fn thread_cache_refuses_a_double_release_of_a_cached_block() {
    let cache = ThreadCached::new(SystemAllocator::new(), 64, 16, 4);
    let a = cache.allocate(64, 16).unwrap();
    // SAFETY: released once; the block is now cached.
    unsafe { cache.deallocate(a).unwrap() };
    // SAFETY: deliberate double release; refused by the cross-slot check.
    assert_eq!(unsafe { cache.deallocate(a) }, Err(AllocError::DoubleFree));
    cache.flush().unwrap();
}

#[test]
fn bulk_memory_helpers_match_byte_loops() {
    for offset in 0..8 {
        for len in [0usize, 1, 7, 8, 9, 63, 100] {
            let mut buf = vec![0u8; len + 16];
            // SAFETY: the range `[offset, offset + len)` lies inside `buf`.
            unsafe { mem::fill(buf.as_mut_ptr().add(offset), 0x5A, len) };
            assert!(buf[offset..offset + len].iter().all(|&b| b == 0x5A));
            assert!(buf[..offset].iter().all(|&b| b == 0));
            // SAFETY: as above.
            unsafe { mem::zero(buf.as_mut_ptr().add(offset), len) };
            assert!(buf[offset..offset + len].iter().all(|&b| b == 0));
        }
    }
    let mut data: Vec<u8> = (0..=255u8).cycle().take(300).collect();
    let expect = data.clone();
    // Overlapping forward copy must behave like memmove.
    // SAFETY: both ranges lie inside `data`.
    unsafe { mem::copy(data.as_mut_ptr().add(5), data.as_ptr(), 200) };
    assert_eq!(&data[5..205], &expect[..200]);
    let src: Vec<u8> = (1..=64u8).collect();
    let mut dst = vec![0u8; 64];
    // SAFETY: both buffers hold 64 bytes.
    unsafe { mem::copy(dst.as_mut_ptr(), src.as_ptr(), 64) };
    assert_eq!(dst, src);
    assert!(mem::is_aligned(dst.as_ptr().wrapping_add(0), 1));
}

#[test]
fn crc32_matches_the_standard_check_value() {
    assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    assert_eq!(crc32(b""), 0);
    let whole = crc32(b"hello world");
    let split = crc32_update(crc32(b"hello "), b"world");
    assert_eq!(whole, split);
}

#[test]
fn hash64_is_deterministic_and_sensitive() {
    let a = hash64(b"quantum", 1);
    assert_eq!(a, hash64(b"quantum", 1));
    assert_ne!(a, hash64(b"quantum", 2));
    assert_ne!(a, hash64(b"quantun", 1));
    assert_ne!(hash64(b"", 0), hash64(b"\0", 0));
}

#[test]
fn zero_on_free_wipes_the_payload_of_a_released_tlsf_block() {
    let heap = Tlsf::new(4096).unwrap();
    heap.set_zero_on_free(true);
    assert!(heap.zero_on_free());
    let block = heap.allocate(256, 16).unwrap();
    // SAFETY: the block is live and 256 bytes long.
    unsafe { block.as_mut_slice().fill(0xCC) };
    let end = block.ptr().wrapping_add(block.size());
    // SAFETY: release the block; the heap owns the region for the whole test, so the freed bytes stay mapped
    // and the check below reads memory the heap still owns.
    unsafe { heap.deallocate(block).unwrap() };
    // SAFETY: the last eight payload bytes are outside every header and free-list link, and still mapped.
    let tail = unsafe { core::slice::from_raw_parts(end.wrapping_sub(8), 8) };
    assert!(tail.iter().all(|&b| b == 0));
    heap.verify().unwrap();
}

#[test]
fn pool_zeroing_refuses_misuse_and_wipes_live_blocks() {
    struct Probe<'a> {
        pool: &'a PoolAllocator,
        zero_seen: std::cell::Cell<bool>,
    }
    impl Allocator for Probe<'_> {
        fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
            self.pool.allocate(size, align)
        }
        unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
            // SAFETY: the block is still live at this point; it is read before release.
            let bytes = unsafe { core::slice::from_raw_parts(block.ptr(), block.size()) };
            self.zero_seen.set(bytes.iter().all(|&b| b == 0));
            // SAFETY: the caller guarantees the block is live and from this allocator.
            unsafe { self.pool.deallocate(block) }
        }
        fn capabilities(&self) -> Capabilities {
            self.pool.capabilities()
        }
    }
    impl LiveBlocks for Probe<'_> {
        fn is_live(&self, block: Block) -> bool {
            self.pool.is_live(block)
        }
    }

    let pool = PoolAllocator::create(32, 16, 4).unwrap();
    let probe = Probe { pool: &pool, zero_seen: std::cell::Cell::new(false) };
    let zeroing = Zeroing::new(&probe);
    let block = zeroing.allocate(32, 16).unwrap();
    // SAFETY: the block is live and 32 bytes long.
    unsafe { block.as_mut_slice().fill(0x77) };
    // SAFETY: a live block released once.
    unsafe { zeroing.deallocate(block).unwrap() };
    assert!(probe.zero_seen.get());

    let live = zeroing.allocate(32, 16).unwrap();
    // SAFETY: the block is live and 32 bytes long.
    unsafe { live.as_mut_slice().fill(0x42) };
    // SAFETY: deliberate misuse: an interior pointer that the zeroing decorator must not touch.
    let interior = unsafe { Block::from_raw_parts(live.ptr().wrapping_add(8), 8, 8) };
    // SAFETY: refused before any write.
    assert!(unsafe { zeroing.deallocate(interior) }.is_err());
    // SAFETY: the block is live and 32 bytes long.
    assert!(unsafe { live.as_mut_slice() }.iter().all(|&b| b == 0x42));
    // SAFETY: a live block released once.
    unsafe { zeroing.deallocate(live).unwrap() };
}

#[test]
fn large_page_reservation_reports_what_it_obtained() {
    let arena = HugeArena::new(4 << 20).unwrap();
    let kind = arena.page_kind();
    assert!(matches!(kind, PageKind::Large | PageKind::Normal));
    eprintln!("huge arena backed by {kind:?} pages on this machine");
    let block = arena.allocate(4096, 64).unwrap();
    assert_eq!(block.ptr() as usize % 64, 0);
}
