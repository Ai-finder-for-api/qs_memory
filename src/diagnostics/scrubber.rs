//! Background scrubber (feature 30). Runs an integrity check on a separate thread at a fixed interval and
//! counts passes and failures. The check is supplied by the caller, so it can lock whatever it needs to
//! inspect the heap safely (for example `Tlsf::verify` behind a `Mutex`).

use crate::foundation::error::{AllocError, AllocResult};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// A running integrity checker. Stops and joins its thread on drop.
pub struct Scrubber {
    stop: Arc<AtomicBool>,
    passes: Arc<AtomicUsize>,
    failures: Arc<AtomicUsize>,
    last_error: Arc<std::sync::Mutex<Option<AllocError>>>,
    handle: Option<JoinHandle<()>>,
}

impl Scrubber {
    /// Starts a thread that calls `check` every `interval` until the scrubber is dropped.
    pub fn spawn<F>(interval: Duration, mut check: F) -> Self
    where
        F: FnMut() -> AllocResult<()> + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let passes = Arc::new(AtomicUsize::new(0));
        let failures = Arc::new(AtomicUsize::new(0));
        let last_error = Arc::new(std::sync::Mutex::new(None));
        let (s, p, f, e) = (stop.clone(), passes.clone(), failures.clone(), last_error.clone());
        let step = interval.min(Duration::from_millis(10)).max(Duration::from_millis(1));
        let handle = thread::spawn(move || {
            let mut waited = Duration::ZERO;
            while !s.load(Ordering::Acquire) {
                thread::sleep(step);
                waited += step;
                if waited < interval {
                    continue;
                }
                waited = Duration::ZERO;
                p.fetch_add(1, Ordering::Relaxed);
                if let Err(err) = check() {
                    f.fetch_add(1, Ordering::Relaxed);
                    *e.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(err);
                }
            }
        });
        Scrubber { stop, passes, failures, last_error, handle: Some(handle) }
    }

    /// Checks completed so far.
    pub fn passes(&self) -> usize {
        self.passes.load(Ordering::Relaxed)
    }

    /// Checks that reported an error.
    pub fn failures(&self) -> usize {
        self.failures.load(Ordering::Relaxed)
    }

    /// The most recent error reported by a check, if any.
    pub fn last_error(&self) -> Option<AllocError> {
        *self.last_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Stops the scrubber and waits for its thread to finish.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Scrubber {
    fn drop(&mut self) {
        self.shutdown();
    }
}
