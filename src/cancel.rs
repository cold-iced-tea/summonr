//! The cancel signal of the run a thread is working for. A run sets it on
//! its own thread (and each parallel worker on its own) for as long as it
//! executes; blocking work deep in a reagent (a child process, an MCP or
//! wasm server, a model request) polls it, so `stop` takes effect within a
//! poll interval instead of when the step happens to finish.
use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

thread_local! {
    static CURRENT: RefCell<Option<Arc<AtomicBool>>> = const { RefCell::new(None) };
}

/// Restores the previous signal when dropped (runs nest: a child ritual
/// runs on its parent's thread under the same signal).
pub struct Guard(Option<Arc<AtomicBool>>);

impl Drop for Guard {
    fn drop(&mut self) {
        let prev = self.0.take();
        CURRENT.with(|c| *c.borrow_mut() = prev);
    }
}

/// Work on this thread now belongs to a run cancelled through `flag`.
pub fn enter(flag: Arc<AtomicBool>) -> Guard {
    Guard(CURRENT.with(|c| c.borrow_mut().replace(flag)))
}

/// The current run's signal, to hand to a helper thread.
pub fn current() -> Option<Arc<AtomicBool>> {
    CURRENT.with(|c| c.borrow().clone())
}

/// Whether the run this thread works for has been asked to stop.
pub fn requested() -> bool {
    CURRENT.with(|c| c.borrow().as_ref().is_some_and(|f| f.load(Ordering::SeqCst)))
}
