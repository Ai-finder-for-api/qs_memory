//! Epoch-based reclamation (feature 26). Threads pin the collector while they read shared data. Memory
//! that readers might still see is retired rather than freed. A retired item is run only after the global
//! epoch has advanced twice past the epoch it was retired in, which can only happen once every pinned
//! thread has left the epoch it was pinned in.
//!
//! The global epoch advances only when every pinned participant has caught up with it. This is the standard
//! three-epoch scheme. Participants are fixed at creation; `pin` returns `None` when all are busy.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

const IDLE: usize = usize::MAX;

type Garbage = Box<dyn FnOnce() + Send>;

pub struct Epoch {
    global: AtomicUsize,
    slots: Box<[AtomicUsize]>,
    garbage: Mutex<Vec<(usize, Garbage)>>,
}

/// Keeps the calling participant pinned while alive.
pub struct Guard<'a> {
    epoch: &'a Epoch,
    slot: usize,
}

impl Epoch {
    /// A collector for up to `participants` threads.
    pub fn new(participants: usize) -> Self {
        Epoch {
            global: AtomicUsize::new(0),
            slots: (0..participants).map(|_| AtomicUsize::new(IDLE)).collect(),
            garbage: Mutex::new(Vec::new()),
        }
    }

    /// The current global epoch.
    pub fn epoch(&self) -> usize {
        self.global.load(Ordering::Acquire)
    }

    /// Pins the calling thread to the current epoch. Returns `None` when every participant is busy.
    pub fn pin(&self) -> Option<Guard<'_>> {
        for (slot, state) in self.slots.iter().enumerate() {
            if state.compare_exchange(IDLE, self.epoch(), Ordering::AcqRel, Ordering::Acquire).is_ok() {
                return Some(Guard { epoch: self, slot });
            }
        }
        None
    }

    /// Schedules `f` to run once no pinned thread can still see what it frees.
    pub fn retire<F: FnOnce() + Send + 'static>(&self, f: F) {
        let retired_at = self.epoch();
        self.garbage.lock().unwrap_or_else(PoisonError::into_inner).push((retired_at, Box::new(f)));
    }

    /// Advances the epoch if possible and runs every item that is now safe. Returns how many ran.
    pub fn collect(&self) -> usize {
        let now = self.epoch();
        let all_caught_up = self.slots.iter().all(|s| {
            let v = s.load(Ordering::Acquire);
            v == IDLE || v == now
        });
        if all_caught_up {
            let _ = self.global.compare_exchange(now, now + 1, Ordering::AcqRel, Ordering::Acquire);
        }
        let now = self.epoch();
        let ready: Vec<Garbage> = {
            let mut list = self.garbage.lock().unwrap_or_else(PoisonError::into_inner);
            let mut keep = Vec::new();
            let mut ready = Vec::new();
            for (retired_at, f) in list.drain(..) {
                if retired_at + 2 <= now {
                    ready.push(f);
                } else {
                    keep.push((retired_at, f));
                }
            }
            *list = keep;
            ready
        };
        let count = ready.len();
        for f in ready {
            f();
        }
        count
    }

    /// Items waiting to be collected.
    pub fn pending(&self) -> usize {
        self.garbage.lock().unwrap_or_else(PoisonError::into_inner).len()
    }
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.epoch.slots[self.slot].store(IDLE, Ordering::Release);
    }
}
