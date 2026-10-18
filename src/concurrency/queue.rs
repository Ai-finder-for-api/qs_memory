//! Bounded lock-free queue (feature 24, queue half). Dmitry Vyukov's sequence-numbered ring: each slot
//! carries a sequence number, so producers and consumers claim positions with one compare-and-swap and
//! wait for their turn on the slot. It is safe for many producers and many consumers; a single-consumer
//! MPSC use is the common case.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Slot<T> {
    seq: AtomicUsize,
    value: UnsafeCell<MaybeUninit<T>>,
}

pub struct BoundedQueue<T> {
    buffer: Box<[Slot<T>]>,
    mask: usize,
    enqueue: AtomicUsize,
    dequeue: AtomicUsize,
}

// SAFETY: a slot's value is written only by the producer that won its position, and read only by the consumer
// that won it, with the sequence number carrying the happens-before ordering between them.
unsafe impl<T: Send> Send for BoundedQueue<T> {}
// SAFETY: as above.
unsafe impl<T: Send> Sync for BoundedQueue<T> {}

impl<T> BoundedQueue<T> {
    /// A queue with room for at least `capacity` values (rounded up to a power of two, minimum 2).
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.next_power_of_two().max(2);
        let buffer: Box<[Slot<T>]> = (0..cap)
            .map(|i| Slot { seq: AtomicUsize::new(i), value: UnsafeCell::new(MaybeUninit::uninit()) })
            .collect();
        BoundedQueue { buffer, mask: cap - 1, enqueue: AtomicUsize::new(0), dequeue: AtomicUsize::new(0) }
    }

    /// Number of slots.
    pub fn capacity(&self) -> usize {
        self.buffer.len()
    }

    /// Adds `value`, or returns it back when the queue is full.
    pub fn push(&self, value: T) -> Result<(), T> {
        let mut pos = self.enqueue.load(Ordering::Relaxed);
        loop {
            let slot = &self.buffer[pos & self.mask];
            let seq = slot.seq.load(Ordering::Acquire);
            let diff = seq as isize - pos as isize;
            if diff == 0 {
                match self.enqueue.compare_exchange_weak(pos, pos + 1, Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => {
                        // SAFETY: the successful compare-and-swap gives this producer exclusive use of the slot.
                        unsafe { (*slot.value.get()).write(value) };
                        slot.seq.store(pos + 1, Ordering::Release);
                        return Ok(());
                    }
                    Err(actual) => pos = actual,
                }
            } else if diff < 0 {
                return Err(value);
            } else {
                pos = self.enqueue.load(Ordering::Relaxed);
            }
        }
    }

    /// Removes the oldest value, if any.
    pub fn pop(&self) -> Option<T> {
        let mut pos = self.dequeue.load(Ordering::Relaxed);
        loop {
            let slot = &self.buffer[pos & self.mask];
            let seq = slot.seq.load(Ordering::Acquire);
            let diff = seq as isize - (pos + 1) as isize;
            if diff == 0 {
                match self.dequeue.compare_exchange_weak(pos, pos + 1, Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => {
                        // SAFETY: the successful compare-and-swap gives this consumer exclusive use of the slot,
                        // whose value was written before its sequence number was published.
                        let value = unsafe { (*slot.value.get()).assume_init_read() };
                        slot.seq.store(pos + self.mask + 1, Ordering::Release);
                        return Some(value);
                    }
                    Err(actual) => pos = actual,
                }
            } else if diff < 0 {
                return None;
            } else {
                pos = self.dequeue.load(Ordering::Relaxed);
            }
        }
    }
}

impl<T> Drop for BoundedQueue<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
    }
}
