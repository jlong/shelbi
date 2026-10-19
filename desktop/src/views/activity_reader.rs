//! The Activity feed's background reader (`dt-activity-perf`): the newest
//! window of `events.log`, parsed and with its task titles resolved, read on
//! its own thread and handed to the view as soon as each read lands.
//!
//! - **Woken, not polled.** Each read goes out on an async channel the view
//!   awaits, so a read is folded the moment it lands rather than on the
//!   view's next poll.
//! - **What's at hand first.** A read whose titles wait on the tracker (a
//!   remote board's closed history) is sent twice: once with the titles the
//!   cache and the local board index already hold, then again once the
//!   tracker answers (`read_activity_window_staged`). A read that left ids
//!   for a later batch reads again straight away, so the titles fill in a
//!   batch at a time without waiting out the refresh cadence.
//! - **Built here.** Each read's feed is built on this thread under the
//!   filter the view last showed (`ActivityState`'s [`Read`]), so the UI
//!   thread only compares it with what it shows (most of the 750ms refreshes
//!   read the log as it was) and the relative times refresh off it.
//!
//! The window can make a reader before its Activity view exists (on open),
//! so the first read is already waiting when the view is first shown.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use shelbi_app::activity::{read_activity_window_staged, ActivityFilter, ActivityMetaCache, ACTIVITY_PAGE};

use super::activity::now;
use super::activity_state::{ProjectInfo, Read};

pub struct ActivityReader {
    requests: Option<mpsc::Sender<()>>,
    reads: async_channel::Receiver<Read>,
    /// The line window each read takes; "load older" grows it.
    window: Arc<AtomicUsize>,
    /// The filter the view shows, which each read's feed is built under.
    filter: Arc<Mutex<ActivityFilter>>,
    stop: Arc<AtomicBool>,
}

impl ActivityReader {
    /// Start `project`'s reader and its first read.
    pub fn spawn(project: String) -> Self {
        let (requests, request_rx) = mpsc::channel::<()>();
        let (tx, reads) = async_channel::unbounded::<Read>();
        let window = Arc::new(AtomicUsize::new(ACTIVITY_PAGE));
        let stop = Arc::new(AtomicBool::new(false));
        let filter = Arc::new(Mutex::new(ActivityFilter::default()));
        let (w, s, f) = (window.clone(), stop.clone(), filter.clone());
        let shown = move || f.lock().unwrap_or_else(|p| p.into_inner()).clone();
        std::thread::Builder::new()
            .name("shelbi-activity-read".into())
            .spawn(move || {
                // The title cache lives as long as the reader, so each read
                // only resolves ids it hasn't seen (the TUI shell's rule).
                let cache = Mutex::new(ActivityMetaCache::default());
                let mut again = false;
                loop {
                    if !again && request_rx.recv().is_err() {
                        break;
                    }
                    if s.load(Ordering::Acquire) {
                        break;
                    }
                    while request_rx.try_recv().is_ok() {}
                    let info = ProjectInfo::read(&project);
                    let read = read_activity_window_staged(&project, &cache, w.load(Ordering::Acquire), |early| {
                        let _ = tx.send_blocking(Read::new(early, info.clone(), shown(), now()));
                    });
                    again = read.more_titles;
                    if tx.send_blocking(Read::new(read.data, info, shown(), now())).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn the activity reader");
        let reader = ActivityReader {
            requests: Some(requests),
            reads,
            window,
            filter,
            stop,
        };
        reader.request();
        reader
    }

    /// Ask for a read. Requests that pile up while one is in flight coalesce.
    pub fn request(&self) {
        if let Some(tx) = &self.requests {
            let _ = tx.send(());
        }
    }

    /// Read with a window of `lines` from the next read on.
    pub fn set_window(&self, lines: usize) {
        self.window.store(lines, Ordering::Release);
    }

    /// Build the next reads' feeds under `filter`.
    pub fn set_filter(&self, filter: &ActivityFilter) {
        let mut shown = self.filter.lock().unwrap_or_else(|p| p.into_inner());
        if *shown != *filter {
            *shown = filter.clone();
        }
    }

    /// Where the reads arrive, for the view to await.
    pub fn reads(&self) -> async_channel::Receiver<Read> {
        self.reads.clone()
    }

    /// The newest read that has landed, draining older ones.
    pub fn latest(&self) -> Option<Read> {
        let mut newest = None;
        while let Ok(read) = self.reads.try_recv() {
            newest = Some(read);
        }
        newest
    }

    /// Stop the reader without waiting on a read in flight (its result is
    /// dropped), so the UI thread never joins a slow read.
    pub fn stop_without_waiting(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(tx) = self.requests.take() {
            let _ = tx.send(());
        }
        self.reads.close();
    }
}

impl Drop for ActivityReader {
    fn drop(&mut self) {
        self.stop_without_waiting();
    }
}
