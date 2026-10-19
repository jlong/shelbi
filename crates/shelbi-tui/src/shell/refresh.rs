//! The shell's background refresh worker.
//!
//! The single-process TUI shell must never block its event loop on a disk or
//! `gh` read — one blocked read freezes every terminal view. So all view data is
//! read on this worker thread and published as an immutable [`ShellSnapshot`]; the
//! UI thread only ever folds a ready snapshot in (a cheap in-memory operation) and
//! polls. It mirrors [`shelbi_app::refresh`] (threads + `std::sync::mpsc`, no
//! tokio) but carries the richer data the embedded native views
//! ([`crate::kanban`], [`crate::activity`], [`crate::machines`]) need in addition
//! to the sidebar model.
//!
//! The reader is injected as a closure, so a test can supply a deliberately slow
//! one and assert the loop keeps running (the "no native view blocks the event
//! loop while refreshing" criterion).

use shelbi_app::view::SidebarModel;

use crate::activity::ActivityData;
use crate::kanban::BoardData;
use shelbi_app::view::MachinesModel;

/// One immutable published read of the world for the shell. The native-view
/// fields carry the opaque data bundles the embedded apps fold in via their
/// `apply_*` methods; the sidebar model feeds the renderer directly.
pub struct ShellSnapshot {
    pub sidebar: Option<SidebarModel>,
    pub board: Option<BoardData>,
    pub activity: Option<ActivityData>,
    pub machines: Option<MachinesModel>,
}

/// A refresh worker, generic over the payload `T` it publishes. Dropping it
/// stops the worker and joins its thread. Shared with the desktop board; see
/// [`shelbi_app::refresh::Refresher`].
///
/// The shell runs several: the core refresher ([`ShellSnapshot`] — sidebar, board,
/// machines, all read from the daemon index / disk), a separate activity
/// refresher ([`crate::activity::ActivityData`]), and the closed-page refresher.
/// Splitting them is what keeps a slow activity-title hydration pass from
/// delaying the sidebar: the core snapshot publishes on its own worker.
pub use shelbi_app::refresh::Refresher;

/// Spawn a refresh worker driven by `reader` ([`shelbi_app::refresh::spawn_worker`]):
/// the worker calls `reader(gen)` off the UI thread on each request and
/// publishes the result; requests that pile up while a read is in flight
/// coalesce into one.
pub use shelbi_app::refresh::spawn_worker as spawn;

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn empty(_generation: u64) -> ShellSnapshot {
        ShellSnapshot {
            sidebar: None,
            board: None,
            activity: None,
            machines: None,
        }
    }

    #[test]
    fn request_returns_immediately_even_with_a_slow_reader() {
        // The "no native view blocks the event loop while refreshing" criterion:
        // a reader that takes 150ms must not stall `request()`.
        let handle = super::spawn(|gen| {
            std::thread::sleep(Duration::from_millis(150));
            empty(gen)
        });
        let t0 = Instant::now();
        assert!(handle.request());
        let cost = t0.elapsed();
        assert!(
            cost < Duration::from_millis(50),
            "request() must not block on a slow read, took {cost:?}"
        );
    }

    #[test]
    fn latest_drains_to_the_freshest_snapshot() {
        let handle = super::spawn(empty);
        for _ in 0..5 {
            handle.request();
        }
        std::thread::sleep(Duration::from_millis(100));
        let _latest = handle.latest().expect("a snapshot");
        assert!(handle.latest().is_none(), "drained");
    }

    #[test]
    fn stop_winds_down_the_worker() {
        let mut handle = super::spawn(empty);
        handle.request();
        handle.stop();
        assert!(!handle.request(), "requests refused after stop");
        handle.stop(); // idempotent
    }
}
