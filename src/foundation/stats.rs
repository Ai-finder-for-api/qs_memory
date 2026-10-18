//! Allocation counters.
//!
//! `Stats` is plain data for single-threaded allocators. `AtomicStats` uses relaxed
//! atomics for thread-safe allocators; its `snapshot` is not a consistent cut across
//! fields while other threads are allocating.
//!
//! Definitions:
//! - `allocations`: successful allocations of non-empty blocks.
//! - `deallocations`: successful releases of non-empty blocks.
//! - `failed_allocations`: allocation calls that returned an error.
//! - `bytes_requested`: lifetime sum of successful request sizes.
//! - `bytes_live`: sum of sizes of blocks currently live.
//! - `bytes_peak`: the largest value `bytes_live` has reached.
//! - `capacity_bytes`: bytes the allocator can hand out; 0 when unbounded or unknown.
//! - `accounting_errors`: releases that would have driven `bytes_live` below zero.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Plain counters for single-threaded allocators.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Successful allocations of non-empty blocks.
    pub allocations: u64,
    /// Successful releases of non-empty blocks.
    pub deallocations: u64,
    /// Allocation calls that returned an error.
    pub failed_allocations: u64,
    /// Lifetime sum of successful request sizes.
    pub bytes_requested: u64,
    /// Sum of sizes of live blocks.
    pub bytes_live: u64,
    /// Peak of `bytes_live`.
    pub bytes_peak: u64,
    /// Capacity in bytes (0 = unknown or unbounded).
    pub capacity_bytes: u64,
    /// Releases that would have made `bytes_live` negative.
    pub accounting_errors: u64,
}

impl Stats {
    /// Allocations minus deallocations.
    pub const fn live_allocations(&self) -> u64 {
        self.allocations.saturating_sub(self.deallocations)
    }

    /// Fraction of capacity in use; 0.0 when capacity is unknown.
    pub fn utilization(&self) -> f64 {
        if self.capacity_bytes == 0 {
            0.0
        } else {
            self.bytes_live as f64 / self.capacity_bytes as f64
        }
    }

    /// Records a successful allocation of `n` bytes.
    pub fn on_alloc(&mut self, n: usize) {
        let n = n as u64;
        self.allocations += 1;
        self.bytes_requested += n;
        self.bytes_live += n;
        if self.bytes_live > self.bytes_peak {
            self.bytes_peak = self.bytes_live;
        }
    }

    /// Records a successful release of `n` bytes.
    pub fn on_free(&mut self, n: usize) {
        let n = n as u64;
        self.deallocations += 1;
        if n > self.bytes_live {
            self.accounting_errors += 1;
            self.bytes_live = 0;
        } else {
            self.bytes_live -= n;
        }
    }

    /// Records a failed allocation call.
    pub fn on_failure(&mut self) {
        self.failed_allocations += 1;
    }

    /// Records an in-place resize from `old` to `new` bytes.
    pub fn on_resize(&mut self, old: usize, new: usize) {
        if new >= old {
            let d = (new - old) as u64;
            self.bytes_live += d;
            self.bytes_requested += d;
            if self.bytes_live > self.bytes_peak {
                self.bytes_peak = self.bytes_live;
            }
        } else {
            let d = (old - new) as u64;
            if d > self.bytes_live {
                self.accounting_errors += 1;
                self.bytes_live = 0;
            } else {
                self.bytes_live -= d;
            }
        }
    }
}

/// Thread-safe counters built from relaxed atomics.
#[derive(Debug, Default)]
pub struct AtomicStats {
    allocations: AtomicU64,
    deallocations: AtomicU64,
    failed_allocations: AtomicU64,
    bytes_requested: AtomicU64,
    bytes_live: AtomicU64,
    bytes_peak: AtomicU64,
    capacity_bytes: AtomicU64,
    accounting_errors: AtomicU64,
}

impl AtomicStats {
    /// Zeroed counters.
    pub const fn new() -> Self {
        Self {
            allocations: AtomicU64::new(0),
            deallocations: AtomicU64::new(0),
            failed_allocations: AtomicU64::new(0),
            bytes_requested: AtomicU64::new(0),
            bytes_live: AtomicU64::new(0),
            bytes_peak: AtomicU64::new(0),
            capacity_bytes: AtomicU64::new(0),
            accounting_errors: AtomicU64::new(0),
        }
    }

    /// Sets the capacity counter.
    pub fn set_capacity(&self, bytes: u64) {
        self.capacity_bytes.store(bytes, Relaxed);
    }

    /// Records a successful allocation of `n` bytes.
    pub fn on_alloc(&self, n: usize) {
        let n = n as u64;
        self.allocations.fetch_add(1, Relaxed);
        self.bytes_requested.fetch_add(n, Relaxed);
        let live = self.bytes_live.fetch_add(n, Relaxed) + n;
        let mut peak = self.bytes_peak.load(Relaxed);
        while live > peak {
            match self.bytes_peak.compare_exchange_weak(peak, live, Relaxed, Relaxed) {
                Ok(_) => break,
                Err(observed) => peak = observed,
            }
        }
    }

    /// Records a successful release of `n` bytes.
    pub fn on_free(&self, n: usize) {
        let n = n as u64;
        self.deallocations.fetch_add(1, Relaxed);
        let mut cur = self.bytes_live.load(Relaxed);
        loop {
            let next = cur.saturating_sub(n);
            match self.bytes_live.compare_exchange_weak(cur, next, Relaxed, Relaxed) {
                Ok(_) => {
                    if cur < n {
                        self.accounting_errors.fetch_add(1, Relaxed);
                    }
                    break;
                }
                Err(observed) => cur = observed,
            }
        }
    }

    /// Records a failed allocation call.
    pub fn on_failure(&self) {
        self.failed_allocations.fetch_add(1, Relaxed);
    }

    /// Reads every counter into a plain `Stats`.
    pub fn snapshot(&self) -> Stats {
        Stats {
            allocations: self.allocations.load(Relaxed),
            deallocations: self.deallocations.load(Relaxed),
            failed_allocations: self.failed_allocations.load(Relaxed),
            bytes_requested: self.bytes_requested.load(Relaxed),
            bytes_live: self.bytes_live.load(Relaxed),
            bytes_peak: self.bytes_peak.load(Relaxed),
            capacity_bytes: self.capacity_bytes.load(Relaxed),
            accounting_errors: self.accounting_errors.load(Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_counters_track_live_and_peak() {
        let mut s = Stats::default();
        s.on_alloc(100);
        s.on_alloc(50);
        assert_eq!(s.live_allocations(), 2);
        s.on_free(100);
        assert_eq!(s.bytes_live, 50);
        assert_eq!(s.bytes_peak, 150);
        s.on_free(999);
        assert_eq!(s.accounting_errors, 1);
        assert_eq!(s.bytes_live, 0);
    }

    #[test]
    fn atomic_snapshot_matches_operations() {
        let a = AtomicStats::new();
        a.set_capacity(1000);
        a.on_alloc(300);
        a.on_alloc(200);
        a.on_free(300);
        a.on_failure();
        let s = a.snapshot();
        assert_eq!((s.allocations, s.deallocations, s.failed_allocations), (2, 1, 1));
        assert_eq!((s.bytes_live, s.bytes_peak), (200, 500));
        assert!((s.utilization() - 0.2).abs() < 1e-9);
    }
}
