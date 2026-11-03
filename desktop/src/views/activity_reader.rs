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
//!   tracker answers (the project source's `activity_window`). A read that left ids
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

use shelbi_app::activity::{ActivityFilter, ACTIVITY_PAGE};
use shelbi_app::source::ProjectSource;

use super::activity::now;
use super::activity_state::Read;

/// A test reader's read, run on the asking thread.
#[cfg(test)]
type InlineRead = Box<dyn Fn(&ActivityReader)>;

pub struct ActivityReader {
    requests: Option<mpsc::Sender<()>>,
    reads: async_channel::Receiver<Read>,
    /// The line window each read takes; "load older" grows it.
    window: Arc<AtomicUsize>,
    /// The filter the view shows, which each read's feed is built under.
    filter: Arc<Mutex<ActivityFilter>>,
    stop: Arc<AtomicBool>,
    /// A test's reader reads on the asking thread (a reader thread's send
    /// would wake gpui's test scheduler off its thread).
    #[cfg(test)]
    inline: Option<InlineRead>,
}

impl ActivityReader {
    /// Start the reader over the window's project `source` and its first
    /// read. The source keeps the title cache, so each read only resolves
    /// ids it hasn't seen.
    pub fn spawn(source: Arc<dyn ProjectSource>) -> Self {
        #[cfg(test)]
        if tests::INLINE.with(|i| i.get()) {
            return Self::inline(source);
        }
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
                let mut again = false;
                loop {
                    if !again && request_rx.recv().is_err() {
                        break;
                    }
                    if s.load(Ordering::Acquire) {
                        break;
                    }
                    while request_rx.try_recv().is_ok() {}
                    let read = source.activity_window(w.load(Ordering::Acquire), &mut |early| {
                        let _ = tx.send_blocking(Read::new(early.data, early.info, shown(), now()));
                    });
                    again = read.more_titles;
                    if tx.send_blocking(Read::new(read.data, read.info, shown(), now())).is_err() {
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
            #[cfg(test)]
            inline: None,
        };
        reader.request();
        reader
    }

    /// A reader that reads `source` on the thread that asks ([`tests::INLINE`]).
    #[cfg(test)]
    fn inline(source: Arc<dyn ProjectSource>) -> Self {
        let (tx, reads) = async_channel::unbounded::<Read>();
        let reader = ActivityReader {
            requests: None,
            reads,
            window: Arc::new(AtomicUsize::new(ACTIVITY_PAGE)),
            filter: Arc::new(Mutex::new(ActivityFilter::default())),
            stop: Arc::new(AtomicBool::new(false)),
            inline: Some(Box::new(move |reader: &ActivityReader| {
                let shown = reader.filter.lock().unwrap_or_else(|p| p.into_inner()).clone();
                let read = source.activity_window(reader.window.load(Ordering::Acquire), &mut |_| {});
                let _ = tx.try_send(Read::new(read.data, read.info, shown, now()));
            })),
        };
        reader.request();
        reader
    }

    /// Ask for a read. Requests that pile up while one is in flight coalesce.
    pub fn request(&self) {
        #[cfg(test)]
        if let Some(read) = &self.inline {
            return read(self);
        }
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

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;

    thread_local! {
        /// Readers made on this thread read inline: a gpui test's window
        /// reading a real source sets it.
        pub(crate) static INLINE: Cell<bool> = const { Cell::new(false) };
    }
}
