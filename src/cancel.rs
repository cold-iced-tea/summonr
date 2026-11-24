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
        let _ = CURRENT.try_with(|c| *c.borrow_mut() = prev);
    }
}

/// Work on this thread now belongs to a run cancelled through `flag`.
pub fn enter(flag: Arc<AtomicBool>) -> Guard {
    Guard(
        CURRENT
            .try_with(|c| c.borrow_mut().replace(flag))
            .unwrap_or(None),
    )
}

/// The current run's signal, to hand to a helper thread.
pub fn current() -> Option<Arc<AtomicBool>> {
    CURRENT.try_with(|c| c.borrow().clone()).unwrap_or(None)
}

/// Whether the run this thread works for has been asked to stop.
pub fn requested() -> bool {
    CURRENT
        .try_with(|c| c.borrow().as_ref().is_some_and(|f| f.load(Ordering::SeqCst)))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requested_and_current_do_not_panic_during_tls_teardown() {
        struct OnExit(Arc<AtomicBool>);
        impl Drop for OnExit {
            fn drop(&mut self) {
                assert!(!requested());
                assert!(current().is_none());
                self.0.store(true, Ordering::SeqCst);
            }
        }
        thread_local! {
            static HOOK: RefCell<Option<OnExit>> = const { RefCell::new(None) };
        }
        let ran = Arc::new(AtomicBool::new(false));
        let ran_clone = Arc::clone(&ran);
        std::thread::spawn(move || {
            HOOK.with(|h| *h.borrow_mut() = Some(OnExit(ran_clone)));
            let _g = enter(Arc::new(AtomicBool::new(false)));
            let _ = requested();
        })
        .join()
        .expect("worker thread must not panic during TLS teardown");
        assert!(ran.load(Ordering::SeqCst));
    }
}
