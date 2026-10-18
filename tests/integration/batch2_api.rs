//! Tests for the second feature batch: TLSF, the ring allocator, zero-on-free and the slot map.

use qs_memory::{AllocError, Allocator, Block, RingAllocator, SlotMap, Tlsf};

#[test]
fn tlsf_random_operations_keep_every_invariant() {
    let heap = Tlsf::new(1 << 16).unwrap();
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    let mut live: Vec<Block> = Vec::new();
    for step in 0..3000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        if live.is_empty() || seed % 5 < 3 {
            let size = 1 + ((seed >> 8) as usize) % 1500;
            if let Ok(block) = heap.allocate(size, 16) {
                live.push(block);
            }
        } else {
            let i = ((seed >> 4) as usize) % live.len();
            let block = live.swap_remove(i);
            // SAFETY: the block is live and released once.
            unsafe { heap.deallocate(block).unwrap() };
        }
        if step % 37 == 0 {
            heap.verify().unwrap();
        }
    }
    for block in live.drain(..) {
        // SAFETY: each remaining block is live and released once.
        unsafe { heap.deallocate(block).unwrap() };
    }
    heap.verify().unwrap();
    assert_eq!(heap.free_block_count(), 1);
}

#[test]
fn tlsf_merges_neighbours_back_into_one_block() {
    let heap = Tlsf::new(4096).unwrap();
    let a = heap.allocate(100, 16).unwrap();
    let b = heap.allocate(100, 16).unwrap();
    let c = heap.allocate(100, 16).unwrap();
    // SAFETY: each block is live and released once; the order exercises both merge directions.
    unsafe {
        heap.deallocate(a).unwrap();
        heap.deallocate(c).unwrap();
        heap.verify().unwrap();
        heap.deallocate(b).unwrap();
    }
    heap.verify().unwrap();
    assert_eq!(heap.free_block_count(), 1);
}

#[test]
fn tlsf_refuses_double_free_of_an_isolated_block() {
    let heap = Tlsf::new(4096).unwrap();
    let a = heap.allocate(64, 16).unwrap();
    let b = heap.allocate(64, 16).unwrap();
    let c = heap.allocate(64, 16).unwrap();
    // SAFETY: `b` has allocated neighbours, so it stays isolated; the second release is the misuse under test.
    unsafe {
        heap.deallocate(b).unwrap();
        assert_eq!(heap.deallocate(b), Err(AllocError::DoubleFree));
        heap.deallocate(a).unwrap();
        heap.deallocate(c).unwrap();
    }
    heap.verify().unwrap();
}

#[test]
fn tlsf_refuses_foreign_pointers_and_over_aligned_requests() {
    let heap = Tlsf::new(4096).unwrap();
    assert_eq!(heap.allocate(8, 64).unwrap().ptr() as usize % 64, 0);
    let local = 0u8;
    // SAFETY: the block points at a stack byte outside the region; it is refused without being read.
    let foreign = unsafe { Block::from_raw_parts(&local as *const u8 as *mut u8, 16, 16) };
    // SAFETY: deliberate misuse; refused before any access.
    assert_eq!(unsafe { heap.deallocate(foreign) }, Err(AllocError::NotOwned));
}


#[test]
fn ring_reclaims_in_order_and_wraps_around() {
    let ring = RingAllocator::new(256).unwrap();
    let a = ring.allocate(96, 16).unwrap();
    let b = ring.allocate(96, 16).unwrap();
    let c = ring.allocate(48, 16).unwrap();
    assert_eq!(ring.used_bytes(), 240);
    // SAFETY: `a` is the oldest block, so releasing it is in order.
    unsafe { ring.deallocate(a).unwrap() };
    // A 64-byte block does not fit after `c`, so the 16 spare bytes are skipped and it starts at the front.
    let d = ring.allocate(64, 16).unwrap();
    assert_eq!(d.ptr(), a.ptr());
    // The ring is now full: 96 + 48 + 64 bytes of blocks plus the skipped tail.
    assert_eq!(ring.allocate(48, 16).unwrap_err(), AllocError::CapacityExhausted);
    // SAFETY: in-order release of the remaining blocks.
    unsafe {
        ring.deallocate(b).unwrap();
        ring.deallocate(c).unwrap();
        ring.deallocate(d).unwrap();
    }
    assert_eq!(ring.live_blocks(), 0);
    assert_eq!(ring.used_bytes(), 0);
}

#[test]
fn ring_refuses_out_of_order_release() {
    let ring = RingAllocator::new(256).unwrap();
    let a = ring.allocate(32, 16).unwrap();
    let b = ring.allocate(32, 16).unwrap();
    // SAFETY: `b` is newer than `a`, so this release is the misuse under test and is refused.
    assert_eq!(unsafe { ring.deallocate(b) }, Err(AllocError::OutOfOrderFree));
    // SAFETY: in-order release.
    unsafe {
        ring.deallocate(a).unwrap();
        ring.deallocate(b).unwrap();
    }
}

#[test]
fn slot_map_handles_go_stale_after_removal() {
    let mut map = SlotMap::new();
    let a = map.insert("alpha");
    let b = map.insert("beta");
    assert_eq!(map.get(a), Some(&"alpha"));
    assert_eq!(map.remove(a), Some("alpha"));
    assert_eq!(map.get(a), None);
    assert!(!map.contains(a));
    // The released slot is reused, but the old handle must not alias the new value.
    let c = map.insert("gamma");
    assert_eq!(map.get(a), None);
    assert_eq!(map.get(c), Some(&"gamma"));
    assert_eq!(map.len(), 2);
    assert_eq!(map.iter().count(), 2);
    assert_eq!(map.get(b), Some(&"beta"));
    if let Some(v) = map.get_mut(c) {
        *v = "delta";
    }
    assert_eq!(map.get(c), Some(&"delta"));
    assert_eq!(map.remove(a), None);
}
