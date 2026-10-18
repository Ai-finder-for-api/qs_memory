//! Diagnostics and small utilities. Nothing here allocates.
//!
//! - `SizeHistogram`: allocation sizes in power-of-two buckets.
//! - `Telemetry`: counters, a size histogram and failure counts per error kind.
//! - `format_stats`, `hex_dump`: text rendering into caller-provided byte buffers.
//! - `fnv1a64`, `hash_combine`, `fingerprint`: non-cryptographic hashing.
//! - `defer`: runs a closure when the enclosing scope ends, unless dismissed.

use core::fmt::{self, Write};

use crate::foundation::block::Block;
use crate::foundation::error::{AllocError, AllocResult};
use crate::foundation::stats::Stats;

// ------------------------------------------------------------------ histogram

/// Number of histogram buckets; larger sizes are counted in the last one.
pub const HISTOGRAM_BUCKETS: usize = 48;

/// Allocation sizes in power-of-two buckets. Bucket `i` holds sizes in `(2^(i-1), 2^i]`;
/// bucket 0 holds sizes 0 and 1. Percentiles report a bucket's upper bound, so they
/// over-estimate by at most a factor of two.
#[derive(Clone, Copy, Debug)]
pub struct SizeHistogram {
    counts: [u64; HISTOGRAM_BUCKETS],
    count: u64,
    total: u64,
    max: usize,
}

impl Default for SizeHistogram {
    fn default() -> Self {
        Self::new()
    }
}

impl SizeHistogram {
    /// An empty histogram.
    pub const fn new() -> Self {
        SizeHistogram { counts: [0; HISTOGRAM_BUCKETS], count: 0, total: 0, max: 0 }
    }

    /// Bucket index for `size` (not clamped to the table).
    pub const fn bucket_for(size: usize) -> usize {
        if size <= 1 {
            0
        } else {
            (usize::BITS - (size - 1).leading_zeros()) as usize
        }
    }

    /// Upper bound of bucket `bucket`.
    pub const fn bucket_upper_bound(bucket: usize) -> usize {
        1usize << bucket
    }

    /// Records one allocation of `size` bytes.
    pub fn record(&mut self, size: usize) {
        let b = Self::bucket_for(size).min(HISTOGRAM_BUCKETS - 1);
        self.counts[b] += 1;
        self.count += 1;
        self.total += size as u64;
        if size > self.max {
            self.max = size;
        }
    }

    /// Number of recorded allocations.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Sum of recorded sizes.
    pub fn total_bytes(&self) -> u64 {
        self.total
    }

    /// Largest recorded size.
    pub fn max_size(&self) -> usize {
        self.max
    }

    /// Allocations in bucket `bucket` (0 when out of range).
    pub fn bucket_count(&self, bucket: usize) -> u64 {
        self.counts.get(bucket).copied().unwrap_or(0)
    }

    /// Mean recorded size; 0.0 when empty.
    pub fn mean(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.total as f64 / self.count as f64
        }
    }

    /// Upper bound of the bucket containing percentile `p` (clamped to `[0, 1]`).
    pub fn percentile_upper_bound(&self, p: f64) -> usize {
        if self.count == 0 {
            return 0;
        }
        let p = p.clamp(0.0, 1.0);
        let target = (p * (self.count - 1) as f64) as u64 + 1;
        let mut seen = 0u64;
        for (b, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= target {
                return Self::bucket_upper_bound(b);
            }
        }
        self.max
    }
}

// ------------------------------------------------------------------- writers

/// Writes text into a byte slice, counting everything written even when it does not fit.
struct SliceWriter<'b> {
    buf: &'b mut [u8],
    total: usize,
}

impl Write for SliceWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &byte in s.as_bytes() {
            if self.total + 1 < self.buf.len() {
                self.buf[self.total] = byte;
            }
            self.total += 1;
        }
        Ok(())
    }
}

impl SliceWriter<'_> {
    /// Terminates the text with NUL when there is room, and returns the full length.
    fn finish(self) -> usize {
        if !self.buf.is_empty() {
            let at = self.total.min(self.buf.len() - 1);
            self.buf[at] = 0;
        }
        self.total
    }
}

// ----------------------------------------------------------------- telemetry

/// Counters, a size histogram and failure counts per error kind.
#[derive(Clone, Copy, Debug)]
pub struct Telemetry {
    stats: Stats,
    sizes: SizeHistogram,
    errors: [u64; ERROR_KINDS],
}

const ERROR_KINDS: usize = 16;

impl Default for Telemetry {
    fn default() -> Self {
        Self::new()
    }
}

impl Telemetry {
    /// Empty telemetry.
    pub const fn new() -> Self {
        Telemetry { stats: Stats {
            allocations: 0, deallocations: 0, failed_allocations: 0, bytes_requested: 0,
            bytes_live: 0, bytes_peak: 0, capacity_bytes: 0, accounting_errors: 0,
        }, sizes: SizeHistogram::new(), errors: [0; ERROR_KINDS] }
    }

    /// Records a successful allocation.
    pub fn record_allocation(&mut self, size: usize) {
        self.stats.on_alloc(size);
        self.sizes.record(size);
    }

    /// Records a successful release.
    pub fn record_free(&mut self, size: usize) {
        self.stats.on_free(size);
    }

    /// Records a failed allocation.
    pub fn record_failure(&mut self, e: AllocError) {
        self.stats.on_failure();
        let idx = e.code() as usize;
        if idx < ERROR_KINDS {
            self.errors[idx] += 1;
        }
    }

    /// Records the outcome of an allocation attempt for a request of `requested` bytes.
    pub fn observe<T>(&mut self, result: &AllocResult<T>, requested: usize) {
        match result {
            Ok(_) => self.record_allocation(requested),
            Err(e) => self.record_failure(*e),
        }
    }

    /// Current counters.
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Size distribution.
    pub fn sizes(&self) -> &SizeHistogram {
        &self.sizes
    }

    /// How many allocations failed with `e`.
    pub fn failures_of(&self, e: AllocError) -> u64 {
        self.errors.get(e.code() as usize).copied().unwrap_or(0)
    }

    /// Writes a two-line report into `buf` and returns its full length.
    pub fn report(&self, buf: &mut [u8]) -> usize {
        let mut w = SliceWriter { buf, total: 0 };
        let _ = write_stats(&mut w, &self.stats);
        let _ = w.write_char('\n');
        let _ = write!(
            w,
            "count={} total={} mean={:.1} max={} p50<={} p90<={} p99<={}",
            self.sizes.count(),
            self.sizes.total_bytes(),
            self.sizes.mean(),
            self.sizes.max_size(),
            self.sizes.percentile_upper_bound(0.50),
            self.sizes.percentile_upper_bound(0.90),
            self.sizes.percentile_upper_bound(0.99)
        );
        w.finish()
    }
}

fn write_stats(w: &mut impl Write, s: &Stats) -> fmt::Result {
    write!(
        w,
        "allocs={} frees={} live={} failed={} requested={} live_bytes={} peak_bytes={} capacity={} utilisation={:.1}% accounting_errors={}",
        s.allocations,
        s.deallocations,
        s.live_allocations(),
        s.failed_allocations,
        s.bytes_requested,
        s.bytes_live,
        s.bytes_peak,
        s.capacity_bytes,
        s.utilization() * 100.0,
        s.accounting_errors
    )
}

/// Writes one line of counters into `buf` (NUL-terminated) and returns its full length.
pub fn format_stats(s: &Stats, buf: &mut [u8]) -> usize {
    let mut w = SliceWriter { buf, total: 0 };
    let _ = write_stats(&mut w, s);
    w.finish()
}

/// Writes a 16-bytes-per-row hex dump with an ASCII column into `out` and returns the
/// full length (NUL-terminated when there is room).
pub fn hex_dump(data: &[u8], out: &mut [u8]) -> usize {
    let mut w = SliceWriter { buf: out, total: 0 };
    for (row, chunk) in data.chunks(16).enumerate() {
        let _ = write!(w, "{:08x}: ", row * 16);
        for i in 0..16 {
            match chunk.get(i) {
                Some(b) => {
                    let _ = write!(w, "{:02x} ", b);
                }
                None => {
                    let _ = w.write_str("   ");
                }
            }
            if i == 7 {
                let _ = w.write_char(' ');
            }
        }
        let _ = w.write_char('|');
        for &b in chunk {
            let c = if (0x20..0x7F).contains(&b) { b as char } else { '.' };
            let _ = w.write_char(c);
        }
        let _ = w.write_str("|\n");
    }
    w.finish()
}

// ------------------------------------------------------------------- hashing

/// 64-bit FNV-1a over `data`.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Mixes `v` into a running hash.
pub const fn hash_combine(seed: u64, v: u64) -> u64 {
    seed ^ (v.wrapping_add(0x9E37_79B9_7F4A_7C15).wrapping_add(seed << 6).wrapping_add(seed >> 2))
}

/// Hash of the bytes a block exposes.
///
/// # Safety
/// `b` must be a live block (or empty), so that its `size` bytes are readable.
pub unsafe fn fingerprint(b: Block) -> u64 {
    if b.is_empty() {
        return fnv1a64(&[]);
    }
    // SAFETY: per the contract, `size` bytes starting at `ptr` are readable.
    fnv1a64(unsafe { core::slice::from_raw_parts(b.ptr(), b.size()) })
}

// --------------------------------------------------------------------- defer

/// Runs its closure when dropped, unless dismissed.
#[must_use = "the closure runs when the guard is dropped"]
pub struct Defer<F: FnOnce()> {
    f: Option<F>,
}

impl<F: FnOnce()> Defer<F> {
    /// Keeps the closure from running.
    pub fn dismiss(mut self) {
        self.f = None;
    }
}

impl<F: FnOnce()> Drop for Defer<F> {
    fn drop(&mut self) {
        if let Some(f) = self.f.take() {
            f();
        }
    }
}

/// Returns a guard that runs `f` when the enclosing scope ends.
pub fn defer<F: FnOnce()>(f: F) -> Defer<F> {
    Defer { f: Some(f) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_and_percentiles() {
        assert_eq!(SizeHistogram::bucket_for(0), 0);
        assert_eq!(SizeHistogram::bucket_for(1), 0);
        assert_eq!(SizeHistogram::bucket_for(2), 1);
        assert_eq!(SizeHistogram::bucket_for(3), 2);
        assert_eq!(SizeHistogram::bucket_for(1024), 10);
        assert_eq!(SizeHistogram::bucket_for(1025), 11);
        let mut h = SizeHistogram::new();
        for _ in 0..90 {
            h.record(16);
        }
        for _ in 0..10 {
            h.record(4096);
        }
        assert_eq!(h.count(), 100);
        assert_eq!(h.percentile_upper_bound(0.50), 16);
        assert_eq!(h.percentile_upper_bound(0.99), 4096);
        assert!((h.mean() - 424.0).abs() < 1e-9);
        assert_eq!(SizeHistogram::new().percentile_upper_bound(0.5), 0);
    }

    #[test]
    fn telemetry_aggregates_and_reports() {
        let mut t = Telemetry::new();
        t.record_allocation(16);
        t.record_allocation(4096);
        t.record_free(16);
        t.record_failure(AllocError::CapacityExhausted);
        t.record_failure(AllocError::CapacityExhausted);
        assert_eq!(t.stats().allocations, 2);
        assert_eq!(t.failures_of(AllocError::CapacityExhausted), 2);
        let mut buf = [0u8; 512];
        let n = t.report(&mut buf);
        let text = core::str::from_utf8(&buf[..n]).unwrap();
        assert!(text.contains('\n'));
        assert!(text.contains("count=2"));
    }

    #[test]
    fn report_truncates_without_overflow() {
        let t = Telemetry::new();
        let mut small = [0xFFu8; 16];
        let n = t.report(&mut small);
        assert!(n > small.len());
        assert_eq!(small[15], 0);
        let mut none: [u8; 0] = [];
        assert!(t.report(&mut none) > 0);
    }

    #[test]
    fn hex_dump_formats_rows() {
        let data: Vec<u8> = (0u8..19).collect();
        let mut out = [0u8; 512];
        let n = hex_dump(&data, &mut out);
        let text = core::str::from_utf8(&out[..n]).unwrap();
        assert!(text.starts_with("00000000: 00 01 02"));
        assert!(text.contains("00000010:"));
        assert!(text.contains("|................|"));
        let mut tiny = [0u8; 8];
        assert!(hex_dump(&data, &mut tiny) >= 8);
        assert_eq!(tiny[7], 0);
    }

    #[test]
    fn hashing_matches_known_vectors() {
        assert_eq!(fnv1a64(b""), 0xCBF2_9CE4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xAF63_DC4C_8601_EC8C);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_F739_67E8);
        assert_ne!(hash_combine(1, 2), hash_combine(2, 1));
    }

    #[test]
    fn defer_runs_unless_dismissed() {
        let mut ran = 0;
        {
            let _d = defer(|| ran += 1);
        }
        assert_eq!(ran, 1);
        let mut ran2 = 0;
        {
            let d = defer(|| ran2 += 1);
            d.dismiss();
        }
        assert_eq!(ran2, 0);
    }
}
pub mod checksum;
pub mod mem;
