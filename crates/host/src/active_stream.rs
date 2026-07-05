//! Single-active-viewer enforcement for the shared encoder.
//!
//! The host's encoder (Desktop Duplication is single-instance, and so is
//! `MockEncoder`) only supports one real viewer at a time. Generic over the
//! connection type (`C`) purely so the concurrency-critical handoff logic
//! here — the actual thing worth guarding with a fast test — can be unit
//! tested without spinning up a real QUIC connection; production code only
//! ever uses `quinn::Connection`.

use std::net::SocketAddr;
use std::sync::Mutex;

/// Minimal connection behavior [`ActiveStreamTracker`] needs. Implemented
/// for `quinn::Connection` in `server.rs`; tests provide a trivial fake.
pub(crate) trait CloseableConn: Clone + Send + Sync + 'static {
    fn close_stream(&self);
}

struct Entry<C> {
    addr: SocketAddr,
    conn: C,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// Tracks which connection currently holds the shared encoder.
pub(crate) struct ActiveStreamTracker<C> {
    active: Mutex<Option<Entry<C>>>,
    /// Called when a connection is superseded, purely for the `log!`-style
    /// diagnostic line — kept out of this module so it stays free of the
    /// `server.rs` logging macro and fully independent/testable.
    on_supersede: fn(SocketAddr, SocketAddr),
}

impl<C: CloseableConn> ActiveStreamTracker<C> {
    pub(crate) fn new(on_supersede: fn(SocketAddr, SocketAddr)) -> Self {
        Self {
            active: Mutex::new(None),
            on_supersede,
        }
    }

    /// Claim the shared encoder for `(addr, conn)`. If a *different*
    /// connection currently holds it, closes that connection and **waits
    /// for its task to fully exit** before returning.
    ///
    /// That wait is the important part. An earlier version of this closed
    /// the old connection and immediately recorded the new one as active —
    /// but the consumer's "is this still the active stream" check only runs
    /// once per loop iteration, and a reset-then-read (e.g. force a
    /// keyframe, then read the next encoded frame) is two *separate* lock
    /// acquisitions on the shared encoder, not one atomic step. If the old
    /// task was still mid-iteration when the new session reset the encoder,
    /// it could still advance it once more in between — right back out
    /// from under the new session's own first read. Confirmed via
    /// `known_client_reconnect_without_qr` in `crates/devtools`: 100%
    /// reproducible, not flaky, because the timing asymmetry between a
    /// paced streaming loop and a fresh connection handshake is consistent
    /// run to run. See `tests::superseding_waits_for_old_task_to_finish`
    /// for the fast, in-process regression test.
    ///
    /// Idempotent for repeat calls from the same connection.
    pub(crate) async fn start_streaming(&self, addr: SocketAddr, conn: &C) {
        let old_task = {
            let mut active = self.active.lock().unwrap();
            match active.as_ref() {
                Some(prev) if prev.addr == addr => return,
                Some(prev) => {
                    (self.on_supersede)(prev.addr, addr);
                    prev.conn.close_stream();
                    active.take().and_then(|p| p.task)
                }
                None => None,
            }
        };
        if let Some(task) = old_task {
            let _ = task.await;
        }
        *self.active.lock().unwrap() = Some(Entry {
            addr,
            conn: conn.clone(),
            task: None,
        });
    }

    /// Record the task handle for `addr` once it's spawned (can't be known
    /// at `start_streaming` time, before the task exists). A no-op if
    /// `addr` was already superseded in the meantime.
    pub(crate) fn set_stream_task(&self, addr: SocketAddr, task: tokio::task::JoinHandle<()>) {
        let mut active = self.active.lock().unwrap();
        if let Some(entry) = active.as_mut()
            && entry.addr == addr
        {
            entry.task = Some(task);
        }
    }

    /// Release the shared encoder if `addr` is still the connection holding
    /// it (a no-op if it was already superseded by a newer session).
    pub(crate) fn clear_active_stream(&self, addr: SocketAddr) {
        let mut active = self.active.lock().unwrap();
        if active.as_ref().is_some_and(|a| a.addr == addr) {
            *active = None;
        }
    }

    /// True if `addr` is still the connection holding the shared encoder —
    /// an immediate, in-memory check meant to be used instead of relying
    /// solely on a network-level close signal (e.g. QUIC's
    /// `conn.close_reason()`), which reflects the close *handshake*, not
    /// the moment a connection was told it's being superseded.
    pub(crate) fn is_active_stream(&self, addr: SocketAddr) -> bool {
        self.active
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|a| a.addr == addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[derive(Clone)]
    struct TestConn(Arc<AtomicBool>);

    impl CloseableConn for TestConn {
        fn close_stream(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    fn noop_on_supersede(_old: SocketAddr, _new: SocketAddr) {}

    #[tokio::test]
    async fn first_session_claims_without_closing_anything() {
        let tracker = ActiveStreamTracker::<TestConn>::new(noop_on_supersede);
        let closed = Arc::new(AtomicBool::new(false));
        let conn = TestConn(closed.clone());

        tracker.start_streaming(addr(1), &conn).await;

        assert!(tracker.is_active_stream(addr(1)));
        assert!(!closed.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn repeat_call_from_same_addr_is_idempotent() {
        let tracker = ActiveStreamTracker::<TestConn>::new(noop_on_supersede);
        let closed = Arc::new(AtomicBool::new(false));
        let conn = TestConn(closed.clone());

        tracker.start_streaming(addr(1), &conn).await;
        tracker.start_streaming(addr(1), &conn).await;

        assert!(
            !closed.load(Ordering::SeqCst),
            "must not close its own connection"
        );
    }

    /// The actual regression test for the race found via
    /// `known_client_reconnect_without_qr`: `start_streaming` must not
    /// return until the superseded connection's task has genuinely
    /// finished, not just been asked to stop.
    #[tokio::test]
    async fn superseding_waits_for_old_task_to_finish() {
        let tracker = Arc::new(ActiveStreamTracker::<TestConn>::new(noop_on_supersede));
        let closed1 = Arc::new(AtomicBool::new(false));
        let conn1 = TestConn(closed1.clone());
        tracker.start_streaming(addr(1), &conn1).await;

        // Simulate session 1's still-running loop: it only flips `noticed`
        // once it actually observes it's no longer the active stream,
        // polling like a real per-iteration check would, with a delay long
        // enough that a regressed (non-waiting) `start_streaming` would
        // reliably return *before* this finishes.
        let noticed = Arc::new(AtomicBool::new(false));
        let noticed_for_task = noticed.clone();
        let tracker_for_task = tracker.clone();
        let task = tokio::spawn(async move {
            loop {
                if !tracker_for_task.is_active_stream(addr(1)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            noticed_for_task.store(true, Ordering::SeqCst);
        });
        tracker.set_stream_task(addr(1), task);

        let conn2 = TestConn(Arc::new(AtomicBool::new(false)));
        tracker.start_streaming(addr(2), &conn2).await;

        assert!(
            noticed.load(Ordering::SeqCst),
            "start_streaming returned before the superseded session's task finished"
        );
        assert!(
            closed1.load(Ordering::SeqCst),
            "old connection was not closed"
        );
        assert!(tracker.is_active_stream(addr(2)));
        assert!(!tracker.is_active_stream(addr(1)));
    }

    #[tokio::test]
    async fn clear_is_noop_if_already_superseded() {
        let tracker = ActiveStreamTracker::<TestConn>::new(noop_on_supersede);
        let conn1 = TestConn(Arc::new(AtomicBool::new(false)));
        tracker.start_streaming(addr(1), &conn1).await;
        let conn2 = TestConn(Arc::new(AtomicBool::new(false)));
        tracker.start_streaming(addr(2), &conn2).await;

        // addr(1)'s cleanup (e.g. a dropped connection guard) fires late,
        // after addr(2) already took over — must not clobber addr(2).
        tracker.clear_active_stream(addr(1));

        assert!(tracker.is_active_stream(addr(2)));
    }

    #[tokio::test]
    async fn clear_releases_the_current_holder() {
        let tracker = ActiveStreamTracker::<TestConn>::new(noop_on_supersede);
        let conn = TestConn(Arc::new(AtomicBool::new(false)));
        tracker.start_streaming(addr(1), &conn).await;

        tracker.clear_active_stream(addr(1));

        assert!(!tracker.is_active_stream(addr(1)));
    }
}
