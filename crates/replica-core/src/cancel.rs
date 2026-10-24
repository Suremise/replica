//! Cancelling a run: the tray's Stop.
//!
//! A [`CancelToken`] is shared between whoever may cancel (the tray, a signal handler) and the
//! work that checks it (the restic driver, the mirror engine). Clones share one state.

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Asks running work to stop. Cheap to clone; every clone cancels the same work.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<(Mutex<bool>, Condvar)>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the work to stop. Calling it again does nothing.
    pub fn cancel(&self) {
        let (flag, cond) = &*self.0;
        *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cond.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        *self.0.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Waits until cancelled or `timeout` passes, and says whether it was cancelled. It returns
    /// as soon as [`CancelToken::cancel`] is called.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let (flag, cond) = &*self.0;
        let guard = flag.lock().unwrap_or_else(|e| e.into_inner());
        let (guard, _) = cond
            .wait_timeout_while(guard, timeout, |cancelled| !*cancelled)
            .unwrap_or_else(|e| e.into_inner());
        *guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Instant;

    #[test]
    fn clones_share_the_state() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
        assert!(
            b.wait_timeout(Duration::from_secs(60)),
            "already cancelled returns at once"
        );
    }

    #[test]
    fn a_wait_ends_when_cancelled_from_another_thread() {
        let token = CancelToken::new();
        let waiter = token.clone();
        let start = Instant::now();
        let handle = thread::spawn(move || waiter.wait_timeout(Duration::from_secs(60)));
        token.cancel();
        assert!(handle.join().unwrap());
        assert!(start.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn a_wait_times_out_when_not_cancelled() {
        assert!(!CancelToken::new().wait_timeout(Duration::from_millis(10)));
    }
}
