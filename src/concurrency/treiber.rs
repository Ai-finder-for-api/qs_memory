//! Lock-free stack (feature 24). A fixed-capacity Treiber stack. Nodes are slots in a preallocated array;
//! the free slots form a second stack. Both list heads carry a 32-bit tag that changes on every update,
//! which prevents the ABA problem: a stale compare-and-swap fails instead of corrupting the list.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const NIL: u32 = u32::MAX;

fn pack(tag: u32, index: u32) -> u64 {
    ((tag as u64) << 32) | index as u64
}

fn unpack(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

pub struct LockFreeStack<T> {
    values: Box<[UnsafeCell<MaybeUninit<T>>]>,
    next: Box<[AtomicU32]>,
    head: AtomicU64,
    free: AtomicU64,
}

// SAFETY: slots are handed from one owner to another only through successful compare-and-swap operations
// with acquire/release ordering, so a value is never accessed by two threads at once.
unsafe impl<T: Send> Send for LockFreeStack<T> {}
// SAFETY: as above; the stack only moves values between threads, it never shares references to them.
unsafe impl<T: Send> Sync for LockFreeStack<T> {}

impl<T> LockFreeStack<T> {
    /// A stack holding up to `capacity` values (at most `u32::MAX - 1`).
    pub fn new(capacity: usize) -> Self {
        assert!(capacity < NIL as usize, "capacity too large");
        let values: Box<[UnsafeCell<MaybeUninit<T>>]> =
            (0..capacity).map(|_| UnsafeCell::new(MaybeUninit::uninit())).collect();
        let next: Box<[AtomicU32]> = (0..capacity)
            .map(|i| AtomicU32::new(if i + 1 < capacity { (i + 1) as u32 } else { NIL }))
            .collect();
        let first_free = if capacity == 0 { NIL } else { 0 };
        LockFreeStack {
            values,
            next,
            head: AtomicU64::new(pack(0, NIL)),
            free: AtomicU64::new(pack(0, first_free)),
        }
    }

    fn pop_list(list: &AtomicU64, next: &[AtomicU32]) -> Option<u32> {
        loop {
            let cur = list.load(Ordering::Acquire);
            let (tag, index) = unpack(cur);
            if index == NIL {
                return None;
            }
            let after = next[index as usize].load(Ordering::Relaxed);
            if list
                .compare_exchange_weak(cur, pack(tag.wrapping_add(1), after), Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return Some(index);
            }
        }
    }

    fn push_list(list: &AtomicU64, next: &[AtomicU32], index: u32) {
        loop {
            let cur = list.load(Ordering::Acquire);
            let (tag, head) = unpack(cur);
            next[index as usize].store(head, Ordering::Relaxed);
            if list
                .compare_exchange_weak(cur, pack(tag.wrapping_add(1), index), Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    /// Pushes `value`, or returns it back when the stack is full.
    pub fn push(&self, value: T) -> Result<(), T> {
        let Some(index) = Self::pop_list(&self.free, &self.next) else {
            return Err(value);
        };
        // SAFETY: the slot was taken off the free list, so no other thread can reach it until it is pushed.
        unsafe { (*self.values[index as usize].get()).write(value) };
        Self::push_list(&self.head, &self.next, index);
        Ok(())
    }

    /// Pops the most recently pushed value.
    pub fn pop(&self) -> Option<T> {
        let index = Self::pop_list(&self.head, &self.next)?;
        // SAFETY: the slot was taken off the used list, so this thread owns its initialised value.
        let value = unsafe { (*self.values[index as usize].get()).assume_init_read() };
        Self::push_list(&self.free, &self.next, index);
        Some(value)
    }

    /// Number of slots in the stack.
    pub fn capacity(&self) -> usize {
        self.values.len()
    }
}

impl<T> Drop for LockFreeStack<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}
