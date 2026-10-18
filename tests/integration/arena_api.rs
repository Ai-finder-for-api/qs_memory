//! Arena behaviour through the public API: construction, checkpoints, scopes, borrowed storage.

use qs_memory::{AllocError, Allocator, Arena, ArenaScope, Capabilities};
use std::mem::MaybeUninit;

#[test]
fn owned_arena_allocates_aligned_blocks_until_full() {
    let arena = Arena::new(512).unwrap();
    for align in [1usize, 8, 64] {
        let b = arena.allocate(40, align).unwrap();
        assert_eq!(b.ptr() as usize & (align - 1), 0);
    }
    assert!(arena.used() >= 120);
    assert_eq!(arena.allocate(1024, 1).unwrap_err(), AllocError::CapacityExhausted);
}

#[test]
fn borrowed_storage_is_used_in_place() {
    let mut buf = [MaybeUninit::<u8>::uninit(); 256];
    let arena = Arena::from_slice(&mut buf);
    assert_eq!(arena.capacity(), 256);
    let b = arena.allocate(64, 16).unwrap();
    assert!(arena.owns(b.ptr(), 64));
}

#[test]
fn nested_checkpoints_restore_used_bytes() {
    let arena = Arena::new(4096).unwrap();
    arena.allocate(16, 8).unwrap();
    let outer = arena.checkpoint();
    arena.allocate(100, 8).unwrap();
    let inner = arena.checkpoint();
    arena.allocate(200, 8).unwrap();
    arena.rollback(inner).unwrap();
    assert!(arena.used() > 16);
    arena.rollback(outer).unwrap();
    assert_eq!(arena.stats().live_allocations(), 1);
}

#[test]
fn scope_rolls_back_unless_dismissed() {
    let arena = Arena::new(1024).unwrap();
    arena.allocate(8, 8).unwrap();
    let before = arena.used();
    {
        let _scope = ArenaScope::new(&arena);
        arena.allocate(300, 8).unwrap();
    }
    assert_eq!(arena.used(), before);
    let kept = ArenaScope::new(&arena);
    arena.allocate(16, 8).unwrap();
    kept.dismiss();
    assert!(arena.used() > before);
}

#[test]
fn reset_stales_older_checkpoints_and_clears_counters() {
    let arena = Arena::new(256).unwrap();
    arena.allocate(32, 8).unwrap();
    let cp = arena.checkpoint();
    arena.reset();
    assert_eq!(arena.used(), 0);
    assert_eq!(arena.stats().live_allocations(), 0);
    assert_eq!(arena.rollback(cp), Err(AllocError::StaleCheckpoint));
}

#[test]
fn arena_advertises_checkpoint_capabilities_not_individual_free() {
    let arena = Arena::new(64).unwrap();
    let caps = arena.capabilities();
    assert!(caps.contains(Capabilities::ROLLBACK));
    assert!(!caps.contains(Capabilities::INDIVIDUAL_FREE));
    let b = arena.allocate(8, 8).unwrap();
    // SAFETY: the arena refuses individual frees by design, so the block stays valid.
    unsafe { assert_eq!(arena.deallocate(b), Err(AllocError::NotSupported)) };
}
