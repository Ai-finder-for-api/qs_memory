//! Timeline export (feature 28). Records every allocation and release with its time offset and size,
//! and writes them as CSV so usage can be graphed over time. Recording stops at `limit` events so a long
//! run cannot exhaust memory through the recorder itself.

use crate::allocators::traits::Allocator;
use crate::foundation::block::Block;
use crate::foundation::capability::Capabilities;
use crate::foundation::error::AllocResult;
use core::cell::RefCell;
use std::time::Instant;

/// The kind of event recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Alloc,
    Free,
}

/// One recorded event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    pub micros: u64,
    pub op: Op,
    pub bytes: usize,
}

/// Decorator that records a timeline of allocations and releases.
pub struct Timeline<'a, A: Allocator> {
    inner: &'a A,
    origin: Instant,
    limit: usize,
    events: RefCell<Vec<Event>>,
}

impl<'a, A: Allocator> Timeline<'a, A> {
    /// Wraps `inner`, recording at most `limit` events.
    pub fn new(inner: &'a A, limit: usize) -> Self {
        Timeline { inner, origin: Instant::now(), limit, events: RefCell::new(Vec::new()) }
    }

    fn record(&self, op: Op, bytes: usize) {
        let mut events = self.events.borrow_mut();
        if events.len() < self.limit {
            let micros = self.origin.elapsed().as_micros() as u64;
            events.push(Event { micros, op, bytes });
        }
    }

    /// A copy of the recorded events, in order.
    pub fn events(&self) -> Vec<Event> {
        self.events.borrow().clone()
    }

    /// Highest total of live bytes seen across the recorded events.
    pub fn peak_bytes(&self) -> usize {
        let mut live = 0usize;
        let mut peak = 0usize;
        for e in self.events.borrow().iter() {
            match e.op {
                Op::Alloc => live += e.bytes,
                Op::Free => live = live.saturating_sub(e.bytes),
            }
            peak = peak.max(live);
        }
        peak
    }

    /// The timeline as CSV with a header row: `micros,op,bytes`.
    pub fn to_csv(&self) -> String {
        let mut out = String::from("micros,op,bytes\n");
        for e in self.events.borrow().iter() {
            let op = match e.op {
                Op::Alloc => "alloc",
                Op::Free => "free",
            };
            out.push_str(&format!("{},{},{}\n", e.micros, op, e.bytes));
        }
        out
    }
}

impl<A: Allocator> Allocator for Timeline<'_, A> {
    fn allocate(&self, size: usize, align: usize) -> AllocResult<Block> {
        let block = self.inner.allocate(size, align)?;
        if !block.is_empty() {
            self.record(Op::Alloc, block.size());
        }
        Ok(block)
    }

    unsafe fn deallocate(&self, block: Block) -> AllocResult<()> {
        // SAFETY: the caller guarantees the block is live and from the wrapped allocator.
        let result = unsafe { self.inner.deallocate(block) };
        if result.is_ok() && !block.is_empty() {
            self.record(Op::Free, block.size());
        }
        result
    }

    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
}
