//! Decorators: behaviour added around any allocator.

//! Decorators: behaviour added around any allocator.
//!
//! - `Guarded`: canary bytes before and after every block, checked on release.
//! - `Tracking`: records live blocks with a tag, for leak reports grouped by tag.
//! - `Budget`: refuses allocations beyond a fixed number of live bytes.
//! - `FaultInjecting`: fails selected calls on purpose, for testing error paths.
//! - `Fallback`: serves from a primary allocator, then a secondary one.
//! - `Synchronized`: serializes a single-threaded allocator behind a spin lock.
//!
//! Each decorator borrows its upstream allocator and never allocates on its own, except
//! `Tracking`, which keeps its record table in raw memory that it releases on drop.

pub mod tracking;

use core::cell::Cell;
use core::mem::size_of;
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::foundation::align::{align_up, is_valid_alignment};
use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::config::{CANARY_BYTE, DEBUG_CHECKS, FREED_BYTE, MAX_BLOCK_SIZE, UNINIT_BYTE};
use crate::foundation::error::{AllocError, AllocResult};
use crate::foundation::stats::Stats;


/// Anything that can say whether it owns a range of memory.
pub trait Ownership {
    /// True when `[p, p + n)` lies inside memory this allocator manages.
    fn owns(&self, p: *const u8, n: usize) -> bool;
}

macro_rules! impl_ownership {
    ($($t:ty),*) => {
        $(impl Ownership for $t {
            fn owns(&self, p: *const u8, n: usize) -> bool {
                Self::owns(self, p, n)
            }
        })*
    };
}
impl_ownership!(
    crate::allocators::arena::Arena<'_>,
    crate::allocators::stack::StackAllocator<'_>,
    crate::allocators::pool::PoolAllocator,
    crate::allocators::freelist::FreeListAllocator,
    crate::allocators::buddy::BuddyAllocator,
    crate::allocators::slab::SlabAllocator
);

// ------------------------------------------------------------------ Guarded

const GUARD_CANARY: usize = 16;
const GUARD_HEADER: usize = 48;
const GUARD_MAGIC: u64 = 0x5153_4755_4152_4431; // "QSGUARD1"

#[repr(C)]
#[derive(Clone, Copy)]
struct GuardHeader {
    magic: u64,
    requested: usize,
    total: usize,
    front: usize,
    alignment: usize,
    reserved: u64,
}

const _: () = assert!(size_of::<GuardHeader>() == GUARD_HEADER, "guard header layout");

/// Counters kept by a guarded allocator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GuardReport {
    /// Allocations served.
    pub allocations: u64,
    /// Blocks released.
    pub deallocations: u64,
    /// Releases refused because a canary was overwritten.
    pub corruptions: u64,
    /// Releases refused because the header did not match.
    pub rejected_frees: u64,
}

/// Canary-guarded decorator.
pub struct Guarded<'a, A: Allocator> {
    upstream: &'a A,
    stats: Cell<Stats>,
    report: Cell<GuardReport>,
}

impl<'a, A: Allocator> Guarded<'a, A> {
    /// Wraps `upstream`.
    pub fn new(upstream: &'a A) -> Self {
        Guarded { upstream, stats: Cell::new(Stats::default()), report: Cell::new(GuardReport::default()) }
    }

    /// Guard counters.
    pub fn report(&self) -> GuardReport {
        self.report.get()
    }

    /// Current counters.
    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    fn canaries_intact(payload: *const u8, size: usize) -> bool {
        // SAFETY: callers pass a payload whose canaries lie inside its upstream block.
        unsafe {
            (0..GUARD_CANARY).all(|i| {
                *payload.sub(GUARD_CANARY - i) == CANARY_BYTE && *payload.add(size + i) == CANARY_BYTE
            })
        }
    }

    /// Checks the canaries of a live block without releasing it.
    pub fn check(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() || block.ptr().is_null() {
            return Ok(());
        }
        // SAFETY: the block was returned by this guard, so its header precedes the payload.
        let h = unsafe { ptr::read_unaligned(block.ptr().sub(GUARD_CANARY + GUARD_HEADER) as *const GuardHeader) };
        if h.magic != GUARD_MAGIC || h.requested != block.size() {
            return Err(AllocError::InvalidArgument);
        }
        if Self::canaries_intact(block.ptr(), block.size()) { Ok(()) } else { Err(AllocError::Corruption) }
    }
}

impl<A: Allocator> Allocator for Guarded<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if !is_valid_alignment(align) {
            return Err(AllocError::InvalidAlignment);
        }
        if size == 0 {
            return Ok(Block::empty(align));
        }
        if size > MAX_BLOCK_SIZE {
            return Err(AllocError::Overflow);
        }
        let eff = align.max(16);
        let front = align_up(GUARD_HEADER + GUARD_CANARY, eff).ok_or(AllocError::Overflow)?;
        let total = front
            .checked_add(size)
            .and_then(|v| v.checked_add(GUARD_CANARY))
            .ok_or(AllocError::Overflow)?;
        let raw = self.upstream.allocate(total, eff)?;
        let base = raw.ptr();
        let header = GuardHeader { magic: GUARD_MAGIC, requested: size, total, front, alignment: eff, reserved: 0 };
        // SAFETY: the header and both canary regions lie inside the upstream block of `total` bytes.
        unsafe {
            ptr::write_unaligned(base.add(front - GUARD_CANARY - GUARD_HEADER) as *mut GuardHeader, header);
            ptr::write_bytes(base.add(front - GUARD_CANARY), CANARY_BYTE, GUARD_CANARY);
            ptr::write_bytes(base.add(front + size), CANARY_BYTE, GUARD_CANARY);
            if DEBUG_CHECKS {
                ptr::write_bytes(base.add(front), UNINIT_BYTE, size);
            }
        }
        let mut s = self.stats.get();
        s.on_alloc(size);
        self.stats.set(s);
        let mut r = self.report.get();
        r.allocations += 1;
        self.report.set(r);
        // SAFETY: `front < total`, so the payload is inside the upstream block.
        Ok(Block::new(unsafe { base.add(front) }, size, align))
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return if block.ptr().is_null() { Ok(()) } else { Err(AllocError::InvalidArgument) };
        }
        if block.ptr().is_null() {
            return Err(AllocError::InvalidArgument);
        }
        // SAFETY: per the contract, the header precedes a block this guard returned.
        let h = unsafe { ptr::read_unaligned(block.ptr().sub(GUARD_CANARY + GUARD_HEADER) as *const GuardHeader) };
        let mut r = self.report.get();
        if h.magic != GUARD_MAGIC {
            r.rejected_frees += 1;
            self.report.set(r);
            return Err(AllocError::InvalidFree);
        }
        if h.requested != block.size() {
            r.rejected_frees += 1;
            self.report.set(r);
            return Err(AllocError::InvalidArgument);
        }
        if !Self::canaries_intact(block.ptr(), block.size()) {
            r.corruptions += 1;
            self.report.set(r);
            return Err(AllocError::Corruption);
        }
        // SAFETY: the payload is live and inside the upstream block.
        unsafe {
            if DEBUG_CHECKS {
                ptr::write_bytes(block.ptr(), FREED_BYTE, block.size());
            }
            let base = block.ptr().sub(h.front);
            self.upstream.deallocate(Block::from_raw_parts(base, h.total, h.alignment))?;
        }
        let mut s = self.stats.get();
        s.on_free(block.size());
        self.stats.set(s);
        r.deallocations += 1;
        self.report.set(r);
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        self.upstream.capabilities() | Capabilities::STATISTICS
    }
}

// ------------------------------------------------------------------ Budget

/// Refuses allocations that would exceed a live-byte budget.
pub struct Budget<'a, A: Allocator> {
    upstream: &'a A,
    limit: usize,
    live: Cell<usize>,
    refused: Cell<u64>,
}

impl<'a, A: Allocator> Budget<'a, A> {
    /// A budget of `limit_bytes` live bytes over `upstream`.
    pub fn new(upstream: &'a A, limit_bytes: usize) -> Self {
        Budget { upstream, limit: limit_bytes, live: Cell::new(0), refused: Cell::new(0) }
    }

    /// Live bytes under this budget.
    pub fn live_bytes(&self) -> usize {
        self.live.get()
    }

    /// Bytes still available.
    pub fn remaining(&self) -> usize {
        self.limit - self.live.get()
    }

    /// Allocation calls refused for exceeding the budget.
    pub fn refused(&self) -> u64 {
        self.refused.get()
    }
}

impl<A: Allocator> Allocator for Budget<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        if size > self.remaining() && size != 0 {
            self.refused.set(self.refused.get() + 1);
            return Err(AllocError::CapacityExhausted);
        }
        let b = self.upstream.allocate(size, align)?;
        if !b.is_empty() {
            self.live.set(self.live.get() + size);
        }
        Ok(b)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        // SAFETY: forwarded under the same contract.
        let r = unsafe { self.upstream.deallocate(block) };
        if r.is_ok() && !block.is_empty() {
            self.live.set(self.live.get() - block.size());
        }
        r
    }

    fn capabilities(&self) -> Capabilities {
        self.upstream.capabilities()
    }
}

// ------------------------------------------------------------ FaultInjecting

/// Fails selected allocation calls on purpose. Calls are numbered from 1.
pub struct FaultInjecting<'a, A: Allocator> {
    upstream: &'a A,
    calls: Cell<u64>,
    injected: Cell<u64>,
    fail_at: Cell<u64>,
    fail_from: Cell<u64>,
}

impl<'a, A: Allocator> FaultInjecting<'a, A> {
    /// Wraps `upstream` with no faults armed.
    pub fn new(upstream: &'a A) -> Self {
        FaultInjecting {
            upstream,
            calls: Cell::new(0),
            injected: Cell::new(0),
            fail_at: Cell::new(0),
            fail_from: Cell::new(0),
        }
    }

    /// Fails call number `n` with `OutOfMemory`.
    pub fn fail_call(&self, n: u64) {
        self.fail_at.set(n);
    }

    /// Fails every call from number `n` onward.
    pub fn fail_from(&self, n: u64) {
        self.fail_from.set(n);
    }

    /// Disarms all faults.
    pub fn clear(&self) {
        self.fail_at.set(0);
        self.fail_from.set(0);
    }

    /// Allocation calls made so far.
    pub fn calls(&self) -> u64 {
        self.calls.get()
    }

    /// Calls that were failed on purpose.
    pub fn injected(&self) -> u64 {
        self.injected.get()
    }
}

impl<A: Allocator> Allocator for FaultInjecting<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        let n = self.calls.get() + 1;
        self.calls.set(n);
        let hit = (self.fail_at.get() != 0 && n == self.fail_at.get())
            || (self.fail_from.get() != 0 && n >= self.fail_from.get());
        if hit {
            self.injected.set(self.injected.get() + 1);
            return Err(AllocError::OutOfMemory);
        }
        self.upstream.allocate(size, align)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        // SAFETY: forwarded under the same contract.
        unsafe { self.upstream.deallocate(block) }
    }

    fn capabilities(&self) -> Capabilities {
        self.upstream.capabilities()
    }
}

// ------------------------------------------------------------------ Fallback

/// Serves from `primary`, falling back to `secondary` when the primary cannot.
pub struct Fallback<'a, P: Allocator + Ownership, S: Allocator> {
    primary: &'a P,
    secondary: &'a S,
}

impl<'a, P: Allocator + Ownership, S: Allocator> Fallback<'a, P, S> {
    /// Pairs two allocators.
    pub fn new(primary: &'a P, secondary: &'a S) -> Self {
        Fallback { primary, secondary }
    }
}

impl<P: Allocator + Ownership, S: Allocator> Allocator for Fallback<'_, P, S> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        match self.primary.allocate(size, align) {
            Ok(b) => Ok(b),
            Err(e @ (AllocError::InvalidAlignment | AllocError::Overflow)) => Err(e),
            Err(_) => self.secondary.allocate(size, align),
        }
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() || self.primary.owns(block.ptr(), block.size()) {
            // SAFETY: the primary owns this block, so the contract applies to it.
            unsafe { self.primary.deallocate(block) }
        } else {
            // SAFETY: not owned by the primary, so it came from the secondary.
            unsafe { self.secondary.deallocate(block) }
        }
    }

    fn capabilities(&self) -> Capabilities {
        self.primary.capabilities().intersection(self.secondary.capabilities())
    }
}

// --------------------------------------------------------------- Synchronized

/// Spin lock used by `Synchronized`.
struct SpinLock {
    locked: AtomicBool,
}

impl SpinLock {
    const fn new() -> Self {
        SpinLock { locked: AtomicBool::new(false) }
    }

    fn lock(&self) {
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
    }

    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

/// Serializes calls to a single-threaded allocator. Shareable across threads.
pub struct Synchronized<A: Allocator> {
    inner: A,
    lock: SpinLock,
}

// SAFETY: every access to `inner` happens while `lock` is held, so the `Cell`s inside `A`
// are never touched concurrently. Moving `A` between threads is allowed when `A: Send`.
unsafe impl<A: Allocator + Send> Sync for Synchronized<A> {}

impl<A: Allocator> Synchronized<A> {
    /// Wraps `inner`.
    pub fn new(inner: A) -> Self {
        Synchronized { inner, lock: SpinLock::new() }
    }

    /// Runs `f` with the inner allocator under the lock.
    pub fn with<R>(&self, f: impl FnOnce(&A) -> R) -> R {
        self.lock.lock();
        let r = f(&self.inner);
        self.lock.unlock();
        r
    }
}

impl<A: Allocator> Allocator for Synchronized<A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        self.with(|a| a.allocate(size, align))
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        // SAFETY: forwarded under the same contract, with the lock held.
        self.with(|a| unsafe { a.deallocate(block) })
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities() | Capabilities::THREAD_SAFE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allocators::system::SystemAllocator;

    #[test]
    fn guard_detects_overrun_and_keeps_block() {
        let sys = SystemAllocator::new();
        let g = Guarded::new(&sys);
        let b = g.allocate(32, 16).unwrap();
        // SAFETY: deliberate one-byte overrun inside the upstream block, then repaired.
        unsafe {
            *b.ptr().add(32) = 0;
            assert_eq!(g.check(b), Err(AllocError::Corruption));
            assert_eq!(g.deallocate(b), Err(AllocError::Corruption));
            *b.ptr().add(32) = CANARY_BYTE;
            g.deallocate(b).unwrap();
        }
        assert_eq!(g.report().corruptions, 1);
        assert_eq!(g.report().deallocations, 1);
        assert_eq!(sys.stats().live_allocations(), 0);
    }

    #[test]
    fn budget_refuses_and_recovers() {
        let sys = SystemAllocator::new();
        let budget = Budget::new(&sys, 1000);
        let a = budget.allocate(600, 8).unwrap();
        assert_eq!(budget.allocate(500, 8).unwrap_err(), AllocError::CapacityExhausted);
        assert_eq!(budget.refused(), 1);
        // SAFETY: released once.
        unsafe { budget.deallocate(a).unwrap() };
        assert!(budget.allocate(1000, 8).is_ok());
    }

    #[test]
    fn fault_injection_fails_exact_call() {
        let sys = SystemAllocator::new();
        let f = FaultInjecting::new(&sys);
        f.fail_call(2);
        let a = f.allocate(8, 8).unwrap();
        assert_eq!(f.allocate(8, 8).unwrap_err(), AllocError::OutOfMemory);
        let b = f.allocate(8, 8).unwrap();
        assert_eq!(f.injected(), 1);
        // SAFETY: each block is released once.
        unsafe {
            f.deallocate(a).unwrap();
            f.deallocate(b).unwrap();
        }
    }

    #[test]
    fn fallback_routes_by_ownership() {
        let pool = crate::allocators::pool::PoolAllocator::create(32, 16, 1).unwrap();
        let sys = SystemAllocator::new();
        let fb = Fallback::new(&pool, &sys);
        let a = fb.allocate(32, 16).unwrap();
        let b = fb.allocate(32, 16).unwrap();
        assert!(pool.owns(a.ptr(), 32));
        assert!(!pool.owns(b.ptr(), 32));
        // SAFETY: each block came from `fb` and is released once.
        unsafe {
            fb.deallocate(b).unwrap();
            fb.deallocate(a).unwrap();
        }
        assert_eq!(sys.stats().live_allocations(), 0);
        assert_eq!(fb.allocate(8, 3).unwrap_err(), AllocError::InvalidAlignment);
    }

    #[test]
    fn synchronized_survives_threads() {
        let sys = SystemAllocator::new();
        let shared = Synchronized::new(sys);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    for _ in 0..500 {
                        let b = shared.allocate(64, 16).unwrap();
                        // SAFETY: released once by the thread that allocated it.
                        unsafe { shared.deallocate(b).unwrap() };
                    }
                });
            }
        });
        let stats = shared.with(|a| a.stats());
        assert_eq!(stats.allocations, 2000);
        assert_eq!(stats.live_allocations(), 0);
    }

    #[test]
    fn free_helper_is_linked() {
        // Keeps the system-release path referenced in test builds.
        assert!(crate::platform::page_size() >= 4096);
    }
}
pub mod location;
pub mod quarantine;
pub mod watermark;
pub mod leak;
pub mod zeroing;
pub mod locked;
pub mod sampled;
pub mod timeline;
