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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;

use shelbi_app::view::SidebarModel;

use crate::activity::ActivityData;
use crate::kanban::BoardData;
use crate::machines::MachinesData;

/// One immutable published read of the world for the shell. The native-view
/// fields carry the opaque data bundles the embedded apps fold in via their
/// `apply_*` methods; the sidebar model feeds the renderer directly.
pub struct ShellSnapshot {
    pub sidebar: Option<SidebarModel>,
    pub board: Option<BoardData>,
    pub activity: Option<ActivityData>,
    pub machines: Option<MachinesData>,
}

/// A handle to the running refresh worker. Dropping it stops the worker and joins
/// its thread.
pub struct ShellRefresher {
    request_tx: Option<Sender<()>>,
    snapshot_rx: Receiver<ShellSnapshot>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

/// Spawn the refresh worker driven by `reader`. The worker blocks until
/// [`ShellRefresher::request`] asks for a refresh, then calls `reader(gen)` off
/// the UI thread and publishes the returned [`ShellSnapshot`]. Requests that pile
/// up while a read is in flight coalesce into a single refresh.
pub fn spawn<F>(reader: F) -> ShellRefresher
where
    F: Fn(u64) -> ShellSnapshot + Send + 'static,
{
    let (request_tx, request_rx) = mpsc::channel::<()>();
    let (snapshot_tx, snapshot_rx) = mpsc::channel::<ShellSnapshot>();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);

    let join = std::thread::Builder::new()
        .name("shelbi-shell-refresh".to_string())
        .spawn(move || {
            let mut generation: u64 = 0;
            while request_rx.recv().is_ok() {
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                // Coalesce any piled-up requests into one read.
                while request_rx.try_recv().is_ok() {}
                let snapshot = reader(generation);
                generation = generation.wrapping_add(1);
                if snapshot_tx.send(snapshot).is_err() {
                    break;
                }
            }
        })
        .expect("spawn shell refresh worker thread");

    ShellRefresher {
        request_tx: Some(request_tx),
        snapshot_rx,
        stop,
        join: Some(join),
    }
}

impl ShellRefresher {
    /// Ask for a refresh. Returns immediately — the read happens on the worker.
    /// Returns `false` if the worker has stopped.
    pub fn request(&self) -> bool {
        match &self.request_tx {
            Some(tx) => tx.send(()).is_ok(),
            None => false,
        }
    }

    /// The most recent snapshot available without blocking, draining older queued
    /// ones so the caller always gets the freshest. `None` when nothing new has
    /// arrived since the last call.
    pub fn latest(&self) -> Option<ShellSnapshot> {
        let mut newest = None;
        while let Ok(s) = self.snapshot_rx.try_recv() {
            newest = Some(s);
        }
        newest
    }

    /// Stop the worker and join its thread. Idempotent; also runs on drop.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let tx = self.request_tx.take();
        if let Some(tx) = &tx {
            let _ = tx.send(());
        }
        drop(tx);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for ShellRefresher {
    fn drop(&mut self) {
        self.stop();
    }
}

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
