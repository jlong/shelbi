//! Background refresh.
//!
//! State reads (the board, the error log, workspace statuses) can block —
//! they touch disk, and for a remote board they can touch the network. In
//! a single-process UI a blocking read on the UI thread freezes every
//! terminal view, so the reads run on a worker thread that publishes
//! immutable [`Snapshot`]s back over a channel.
//!
//! The worker uses only `std::thread` and `std::sync::mpsc` — no tokio — so
//! a gpui desktop client can consume it exactly like the ratatui TUI. The
//! reader is injected as a closure, which keeps this module free of any
//! `shelbi-state` IO (the host supplies the real reader; tests supply a
//! canned one).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::SystemTime;

use crate::view::{ActivityModel, ErrorLogModel, IssuesModel, MachinesModel, SidebarModel};

/// One immutable published view of the world. A refresh produces exactly
/// one of these. Fields are `Option` so a reader that can't currently
/// produce a surface (e.g. a cold board) can still publish the ones it has.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    /// A monotonically increasing counter the worker stamps on each
    /// snapshot, so a consumer can tell a newer snapshot from an older one
    /// and drop stale ones.
    pub generation: u64,
    /// When the worker finished the read.
    pub taken_at: Option<SystemTime>,
    pub sidebar: Option<SidebarModel>,
    pub issues: Option<IssuesModel>,
    pub activity: Option<ActivityModel>,
    pub machines: Option<MachinesModel>,
    pub errors: Option<ErrorLogModel>,
}

/// A handle to a running refresh worker. Dropping it stops the worker and
/// joins its thread.
pub struct RefreshHandle {
    /// Taken (dropped) on stop so the worker's `recv` ends and the thread
    /// exits.
    request_tx: Option<Sender<()>>,
    snapshot_rx: Receiver<Snapshot>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

/// Spawn a refresh worker driven by `reader`. The worker blocks until
/// [`RefreshHandle::request`] asks for a refresh, then calls `reader(gen)`
/// off the caller's thread and publishes the returned [`Snapshot`].
///
/// `reader` is handed the generation number to stamp onto the snapshot it
/// builds (so the worker never overwrites the field the reader set). The
/// host's reader calls the `shelbi-state` board/error/activity readers and
/// runs the [`view`](crate::view) builders over their output.
pub fn spawn_refresher<F>(reader: F) -> RefreshHandle
where
    F: Fn(u64) -> Snapshot + Send + 'static,
{
    let (request_tx, request_rx) = mpsc::channel::<()>();
    let (snapshot_tx, snapshot_rx) = mpsc::channel::<Snapshot>();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);

    let join = std::thread::Builder::new()
        .name("shelbi-app-refresh".to_string())
        .spawn(move || {
            let mut generation: u64 = 0;
            // One refresh per request. `recv` ends (Err) when every sender
            // is dropped, which is how `stop` / `Drop` wind the thread down.
            while request_rx.recv().is_ok() {
                if worker_stop.load(Ordering::Acquire) {
                    break;
                }
                // Coalesce: if several requests piled up while a read was in
                // flight, drain them and do a single refresh.
                while request_rx.try_recv().is_ok() {}

                let snapshot = reader(generation);
                generation = generation.wrapping_add(1);
                // A closed receiver (consumer went away) just ends the loop.
                if snapshot_tx.send(snapshot).is_err() {
                    break;
                }
            }
        })
        .expect("spawn refresh worker thread");

    RefreshHandle {
        request_tx: Some(request_tx),
        snapshot_rx,
        stop,
        join: Some(join),
    }
}

impl RefreshHandle {
    /// Ask for a refresh. Returns immediately — the read happens on the
    /// worker thread. Multiple requests before a read completes coalesce
    /// into a single refresh. Returns `false` if the worker has stopped.
    pub fn request(&self) -> bool {
        match &self.request_tx {
            Some(tx) => tx.send(()).is_ok(),
            None => false,
        }
    }

    /// The most recent snapshot available without blocking, draining any
    /// older queued snapshots so the caller always gets the freshest one.
    /// `None` when no new snapshot has arrived since the last call.
    pub fn latest(&self) -> Option<Snapshot> {
        let mut newest = None;
        while let Ok(s) = self.snapshot_rx.try_recv() {
            newest = Some(s);
        }
        newest
    }

    /// Block up to `timeout` for the next snapshot. Used by tests and by a
    /// consumer that wants to wait rather than poll.
    pub fn recv_timeout(&self, timeout: std::time::Duration) -> Option<Snapshot> {
        self.snapshot_rx.recv_timeout(timeout).ok()
    }

    /// Stop the worker and join its thread. Idempotent; also runs on drop.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // Drop the request sender so the worker's blocking `recv` ends, then
        // wake it in case it is parked on `recv` right now.
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

impl Drop for RefreshHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    fn canned(generation: u64) -> Snapshot {
        Snapshot {
            generation,
            taken_at: Some(SystemTime::now()),
            sidebar: Some(SidebarModel {
                project_label: "Alpha".into(),
                nav: vec![],
                workspaces: vec![],
                reviews: vec![],
                zen_on: false,
                unread_errors: 0,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn publishes_a_snapshot_on_request() {
        let handle = spawn_refresher(canned);
        assert!(handle.request());
        let snap = handle
            .recv_timeout(Duration::from_secs(2))
            .expect("a snapshot should arrive");
        assert_eq!(snap.sidebar.unwrap().project_label, "Alpha");
    }

    #[test]
    fn request_does_not_block_on_a_slow_read() {
        // A reader that takes 100ms must not stall `request()`.
        let handle = spawn_refresher(|gen| {
            std::thread::sleep(Duration::from_millis(100));
            canned(gen)
        });
        let t0 = Instant::now();
        assert!(handle.request());
        let request_cost = t0.elapsed();
        assert!(
            request_cost < Duration::from_millis(50),
            "request() should return immediately, took {request_cost:?}"
        );
        // The snapshot still arrives, after the read completes.
        let snap = handle.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(snap.taken_at.is_some());
    }

    #[test]
    fn generations_increase_across_refreshes() {
        let handle = spawn_refresher(canned);
        handle.request();
        let a = handle.recv_timeout(Duration::from_secs(2)).unwrap();
        handle.request();
        let b = handle.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(b.generation > a.generation, "{} > {}", b.generation, a.generation);
    }

    #[test]
    fn latest_drains_to_the_freshest_snapshot() {
        let handle = spawn_refresher(canned);
        // Fire several refreshes; give the worker a moment to service them.
        for _ in 0..5 {
            handle.request();
        }
        std::thread::sleep(Duration::from_millis(100));
        let latest = handle.latest().expect("at least one snapshot");
        // Nothing left queued after a drain.
        assert!(handle.latest().is_none());
        assert!(latest.sidebar.is_some());
    }

    #[test]
    fn stop_winds_down_the_worker() {
        static READS: AtomicUsize = AtomicUsize::new(0);
        let mut handle = spawn_refresher(|gen| {
            READS.fetch_add(1, Ordering::SeqCst);
            canned(gen)
        });
        handle.request();
        let _ = handle.recv_timeout(Duration::from_secs(2));
        handle.stop();
        // After stop, requests are refused and the thread is joined.
        assert!(!handle.request());
        // A second stop is a no-op.
        handle.stop();
    }
}
