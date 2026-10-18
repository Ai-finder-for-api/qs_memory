//! Source-location capture. Every live block remembers the file and line that allocated it, so the
//! live set can be reported by call site.
//!
//! The `allocate` method is `#[track_caller]`, so the recorded location is the caller's line, not
//! a line inside this file. Records are kept in a `Vec` on the global heap (not in the wrapped
//! allocator), so the bookkeeping never consumes the memory it is tracking.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::{AllocError, AllocResult};
use core::cell::RefCell;
use core::panic::Location;
use std::collections::BTreeMap;

struct Entry {
    addr: usize,
    size: usize,
    at: &'static Location<'static>,
}

/// Live allocations that share one call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteSummary {
    pub file: &'static str,
    pub line: u32,
    pub count: usize,
    pub bytes: usize,
}

/// Decorator that records the call site of every live block.
pub struct Located<'a, A: Allocator> {
    inner: &'a A,
    live: RefCell<Vec<Entry>>,
}

impl<'a, A: Allocator> Located<'a, A> {
    /// Wraps `inner`.
    pub fn new(inner: &'a A) -> Self {
        Located { inner, live: RefCell::new(Vec::new()) }
    }

    /// Number of live blocks.
    pub fn live_count(&self) -> usize {
        self.live.borrow().len()
    }

    /// Total size of live blocks, in bytes.
    pub fn live_bytes(&self) -> usize {
        self.live.borrow().iter().map(|e| e.size).sum()
    }

    /// Live blocks grouped by call site, largest total first (ties broken by location).
    pub fn sites(&self) -> Vec<SiteSummary> {
        let mut groups: BTreeMap<(&'static str, u32), (usize, usize)> = BTreeMap::new();
        for entry in self.live.borrow().iter() {
            let slot = groups.entry((entry.at.file(), entry.at.line())).or_insert((0, 0));
            slot.0 += 1;
            slot.1 += entry.size;
        }
        let mut out: Vec<SiteSummary> = groups
            .into_iter()
            .map(|((file, line), (count, bytes))| SiteSummary { file, line, count, bytes })
            .collect();
        out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.file.cmp(b.file)).then(a.line.cmp(&b.line)));
        out
    }
}

impl<A: Allocator> Allocator for Located<'_, A> {
    #[track_caller]
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        let at = Location::caller();
        let block = self.inner.allocate(size, align)?;
        if !block.is_empty() {
            self.live.borrow_mut().push(Entry { addr: block.ptr() as usize, size: block.size(), at });
        }
        Ok(block)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        if block.is_empty() {
            return Ok(());
        }
        let addr = block.ptr() as usize;
        let position = self
            .live
            .borrow()
            .iter()
            .position(|e| e.addr == addr)
            .ok_or(AllocError::InvalidFree)?;
        // SAFETY: the caller guarantees `block` is live and from the wrapped allocator; it is only forwarded.
        unsafe { self.inner.deallocate(block)? };
        self.live.borrow_mut().swap_remove(position);
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}

impl<A: Allocator> Located<'_, A> {
    /// The live blocks as a JSON array of `{"addr", "bytes", "file", "line"}` objects, in allocation order of
    /// the internal list. Useful for snapshots that tools can read.
    pub fn snapshot_json(&self) -> String {
        let mut out = String::from("[");
        for (i, entry) in self.live.borrow().iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format!(
                "{{\"addr\":{},\"bytes\":{},\"file\":\"{}\",\"line\":{}}}",
                entry.addr,
                entry.size,
                json_escape(entry.at.file()),
                entry.at.line()
            ));
        }
        out.push(']');
        out
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}
