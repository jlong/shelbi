//! The background refresh pipeline: everything that reads Shelbi state runs
//! here, off gpui's UI thread, and crosses to the foreground as [`FeedEvent`]s
//! over an async channel a window's foreground task awaits.
//!
//! Per project window, one [`ProjectFeed`]:
//!
//! - a [`shelbi_app::spawn_refresher`] worker that runs the injected reader
//!   (the shared `shelbi_orchestrator::sidebar_model::read_sidebar_model`) and
//!   publishes immutable [`Snapshot`]s;
//! - the hub-socket change subscriber ([`shelbi_app::changes`]), which wakes a
//!   refresh whenever the daemon's poller sees a board or workspace change;
//! - a polling fallback: while that subscription is down (no daemon), the feed
//!   refreshes on the TUI's cadence and reports [`DaemonLink::Polling`] so the
//!   sidebar can show it; when the daemon comes back the subscription
//!   reconnects on its own and the link flips back to [`DaemonLink::Live`].
//!
//! App-wide, one [`ControlWatcher`] holds a subscription on the daemon's
//! control socket for [`Notice::Changed`], the only source that sees a
//! mutation on a GitHub-backed board, and wakes the matching project's feed.
//!
//! Session events (title, exit) arrive from the terminal views'
//! `shelbi-client` connections through a feed's [`SessionSink`].
//!
//! Nothing here ever blocks the caller: spawning returns at once, a refresh
//! request is a channel send, and dropping a feed only raises a stop flag (its
//! threads wind down on their own, so a slow read or a 500ms socket slice never
//! stalls a window close or a project switch).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use shelbi_app::{spawn_refresher, Snapshot};
use shelbi_client::{ControlClient, Notice, SessionEvent};

/// Reads one project's snapshot. Blocking (disk, the daemon socket, and an
/// SSH probe for a remote review slot); always called on the refresh worker.
pub type Reader = Arc<dyn Fn(&str, u64) -> Snapshot + Send + Sync>;

/// The production reader: the sidebar model both clients share.
pub fn sidebar_reader() -> Reader {
    Arc::new(|project, generation| Snapshot {
        generation,
        taken_at: Some(std::time::SystemTime::now()),
        sidebar: shelbi_orchestrator::sidebar_model::read_sidebar_model(project),
        ..Default::default()
    })
}

/// Whether the daemon is pushing changes or the feed is polling for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonLink {
    /// The change subscription is open: updates arrive as they happen.
    Live,
    /// The daemon is unreachable: the feed polls, and reconnects on its own.
    Polling,
}

/// What a session reported.
// Produced through `SessionSink`, which the terminal views (dt-chat-view,
// dt-workspace-view) feed; nothing attaches a session yet.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionNoticeKind {
    /// The program set its window title (empty = reset).
    Title(String),
    /// The program exited.
    Exited,
}

/// A session event, tagged with the session it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionNotice {
    pub session: String,
    pub kind: SessionNoticeKind,
}

/// One message from a project's feed to its window.
#[derive(Debug, Clone)]
pub enum FeedEvent {
    /// A fresh read of the project.
    Snapshot(Box<Snapshot>),
    /// The daemon link changed.
    Link(DaemonLink),
    /// A session's title changed or it exited.
    #[allow(dead_code)] // see `SessionNoticeKind`
    Session(SessionNotice),
}

/// Timing for a [`ProjectFeed`].
#[derive(Debug, Clone, Copy)]
pub struct FeedTiming {
    /// The safety refresh while the daemon pushes changes.
    pub live_interval: Duration,
    /// The refresh while polling (the TUI's 750ms cadence).
    pub poll_interval: Duration,
    /// How long the feed thread waits for a snapshot before it re-checks its
    /// wakes, so a requested refresh starts within this much.
    pub slice: Duration,
}

impl Default for FeedTiming {
    fn default() -> Self {
        FeedTiming {
            live_interval: Duration::from_secs(5),
            poll_interval: Duration::from_millis(750),
            slice: Duration::from_millis(25),
        }
    }
}

/// A project's running feed. Dropping it stops the feed without waiting.
pub struct ProjectFeed {
    wake: Sender<()>,
    stop: Arc<AtomicBool>,
    #[allow(dead_code)] // for `session_sink`
    events: UnboundedSender<FeedEvent>,
}

impl ProjectFeed {
    /// The production feed for `project`: the shared sidebar reader, the hub
    /// change subscription, and the default timing.
    pub fn spawn(project: &str) -> (ProjectFeed, UnboundedReceiver<FeedEvent>) {
        let connected = Arc::new(AtomicBool::new(false));
        let (subscription, changes) =
            shelbi_app::changes::spawn_observed(project, connected.clone());
        Self::spawn_with(
            project,
            sidebar_reader(),
            FeedSources {
                connected,
                changes: Some(changes),
                keep_alive: Some(Box::new(subscription)),
            },
            FeedTiming::default(),
        )
    }

    /// A feed over an injected reader and change sources (tests use canned
    /// ones and drive `connected` by hand).
    pub fn spawn_with(
        project: &str,
        reader: Reader,
        sources: FeedSources,
        timing: FeedTiming,
    ) -> (ProjectFeed, UnboundedReceiver<FeedEvent>) {
        let (events_tx, events_rx) = unbounded();
        let (wake_tx, wake_rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let feed = ProjectFeed {
            wake: wake_tx,
            stop: stop.clone(),
            events: events_tx.clone(),
        };
        let project = project.to_string();
        let spawned = thread::Builder::new()
            .name(format!("shelbi-desktop-feed-{project}"))
            .spawn(move || {
                feed_loop(&project, reader, sources, timing, &wake_rx, &events_tx, &stop)
            });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "could not start the refresh feed");
        }
        (feed, events_rx)
    }

    /// Ask for a refresh now. Never blocks; coalesces with any pending one.
    pub fn request(&self) {
        let _ = self.wake.send(());
    }

    /// A handle the terminal views use to report their sessions' events.
    #[allow(dead_code)] // wired by dt-chat-view / dt-workspace-view
    pub fn session_sink(&self) -> SessionSink {
        SessionSink {
            events: self.events.clone(),
            wake: self.wake.clone(),
        }
    }
}

impl Drop for ProjectFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Where a feed's change wakes and daemon-link state come from.
pub struct FeedSources {
    /// `true` while the daemon change subscription is open.
    pub connected: Arc<AtomicBool>,
    /// Change wakes from the daemon; `None` for a feed with no subscription.
    pub changes: Option<Receiver<shelbi_app::changes::ChangeWake>>,
    /// Kept alive by the feed thread and dropped there, so a subscription's
    /// joining `Drop` never runs on the UI thread.
    pub keep_alive: Option<Box<dyn Send>>,
}

fn feed_loop(
    project: &str,
    reader: Reader,
    sources: FeedSources,
    timing: FeedTiming,
    wake_rx: &Receiver<()>,
    events: &UnboundedSender<FeedEvent>,
    stop: &AtomicBool,
) {
    let FeedSources {
        connected,
        changes,
        keep_alive,
    } = sources;
    let reader_project = project.to_string();
    let refresher = spawn_refresher(move |generation| reader(&reader_project, generation));
    refresher.request();
    let mut last_request = Instant::now();
    let mut link: Option<DaemonLink> = None;

    while !stop.load(Ordering::Acquire) {
        let mut wake = false;
        loop {
            match wake_rx.try_recv() {
                Ok(()) => wake = true,
                Err(mpsc::TryRecvError::Empty) => break,
                // Every handle is gone: the window closed.
                Err(mpsc::TryRecvError::Disconnected) => return finish(refresher, keep_alive),
            }
        }
        if let Some(changes) = &changes {
            while changes.try_recv().is_ok() {
                wake = true;
            }
        }

        let now_link = if connected.load(Ordering::Acquire) {
            DaemonLink::Live
        } else {
            DaemonLink::Polling
        };
        if link != Some(now_link) {
            // Reconnecting after an outage may have missed changes: refresh.
            wake |= link.is_some();
            link = Some(now_link);
            if events.unbounded_send(FeedEvent::Link(now_link)).is_err() {
                break;
            }
        }

        let interval = match now_link {
            DaemonLink::Live => timing.live_interval,
            DaemonLink::Polling => timing.poll_interval,
        };
        if wake || last_request.elapsed() >= interval {
            refresher.request();
            last_request = Instant::now();
        }

        if let Some(first) = refresher.recv_timeout(timing.slice) {
            let newest = refresher.latest().unwrap_or(first);
            if events
                .unbounded_send(FeedEvent::Snapshot(Box::new(newest)))
                .is_err()
            {
                break;
            }
        }
    }
    finish(refresher, keep_alive);
}

/// Wind the refresher and subscription down on this (background) thread.
fn finish(refresher: shelbi_app::RefreshHandle, keep_alive: Option<Box<dyn Send>>) {
    drop(refresher);
    drop(keep_alive);
}

/// Reports a session's title and exit into its project's feed.
#[allow(dead_code)] // wired by dt-chat-view / dt-workspace-view
#[derive(Clone)]
pub struct SessionSink {
    events: UnboundedSender<FeedEvent>,
    wake: Sender<()>,
}

#[allow(dead_code)]
impl SessionSink {
    /// Forward the parts of a `shelbi-client` session event the shell cares
    /// about. An exit also refreshes: a session ending changes workspace state.
    pub fn forward(&self, session: &str, event: &SessionEvent) {
        let kind = match event {
            SessionEvent::Title(title) => SessionNoticeKind::Title(title.clone()),
            SessionEvent::Exited(_) => {
                let _ = self.wake.send(());
                SessionNoticeKind::Exited
            }
            _ => return,
        };
        let _ = self.events.unbounded_send(FeedEvent::Session(SessionNotice {
            session: session.to_string(),
            kind,
        }));
    }
}

/// An app-wide subscription on the daemon's control socket that wakes the
/// feed of whichever project a [`Notice::Changed`] names. It subscribes
/// without registering as an attached client: the app's one registration is
/// bootstrap's control watch (`platform::control`), which also handles the
/// detach and relaunch pushes. Reconnects on its own while the daemon is down.
#[derive(Clone)]
pub struct ControlWatcher {
    wakes: Arc<Mutex<HashMap<String, Vec<Sender<()>>>>>,
}

impl ControlWatcher {
    /// Start the watcher thread.
    pub fn spawn() -> ControlWatcher {
        let watcher = ControlWatcher {
            wakes: Arc::new(Mutex::new(HashMap::new())),
        };
        let thread_watcher = watcher.clone();
        let spawned = thread::Builder::new()
            .name("shelbi-desktop-control".into())
            .spawn(move || thread_watcher.run());
        if let Err(e) = spawned {
            tracing::error!(error = %e, "could not start the control-socket watcher");
        }
        watcher
    }

    /// Route `project`'s change notes to `feed`.
    pub fn watch(&self, project: &str, feed: &ProjectFeed) {
        self.wakes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(project.to_string())
            .or_default()
            .push(feed.wake.clone());
    }

    /// Wake every live feed of `project`, pruning closed ones.
    fn wake(&self, project: &str) {
        let mut wakes = self.wakes.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(senders) = wakes.get_mut(project) {
            senders.retain(|tx| tx.send(()).is_ok());
        }
    }

    fn run(&self) {
        loop {
            let connected = shelbi_state::control_socket_path()
                .ok()
                .and_then(|sock| ControlClient::connect(&sock, shelbi_state::CLIENT_VERSION).ok())
                .and_then(|client| client.subscribe().ok());
            if let Some(mut subscription) = connected {
                loop {
                    match subscription.recv() {
                        Ok(Some(Notice::Changed(note))) => self.wake(&note.project),
                        Ok(Some(_)) => {}
                        Ok(None) | Err(_) => break,
                    }
                }
            }
            thread::sleep(Duration::from_secs(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use shelbi_app::view::SidebarModel;

    fn canned(label: &'static str) -> Reader {
        Arc::new(move |project, generation| Snapshot {
            generation,
            sidebar: Some(SidebarModel::skeleton(&format!("{project}:{label}"))),
            ..Default::default()
        })
    }

    fn fast() -> FeedTiming {
        FeedTiming {
            live_interval: Duration::from_secs(60),
            poll_interval: Duration::from_millis(40),
            slice: Duration::from_millis(5),
        }
    }

    fn sources(connected: &Arc<AtomicBool>) -> FeedSources {
        FeedSources {
            connected: connected.clone(),
            changes: None,
            keep_alive: None,
        }
    }

    /// Block (with a deadline) for the next event matching `want`.
    fn next_matching(
        rx: &mut UnboundedReceiver<FeedEvent>,
        want: impl Fn(&FeedEvent) -> bool,
    ) -> FeedEvent {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "timed out waiting for a feed event");
            match rx.try_recv() {
                Ok(ev) if want(&ev) => return ev,
                Ok(_) => {}
                Err(futures::channel::mpsc::TryRecvError::Closed) => panic!("feed closed"),
                Err(_) => thread::sleep(Duration::from_millis(2)),
            }
        }
    }

    fn is_snapshot(ev: &FeedEvent) -> bool {
        matches!(ev, FeedEvent::Snapshot(_))
    }

    #[test]
    fn spawning_and_requesting_never_block_on_a_slow_read() {
        // An unreachable SSH host makes a read take seconds; the window's
        // thread must not feel it.
        let slow: Reader = Arc::new(|project, generation| {
            thread::sleep(Duration::from_millis(400));
            Snapshot {
                generation,
                sidebar: Some(SidebarModel::skeleton(project)),
                ..Default::default()
            }
        });
        let connected = Arc::new(AtomicBool::new(true));
        let t0 = Instant::now();
        let (feed, rx) = ProjectFeed::spawn_with("demo", slow, sources(&connected), fast());
        feed.request();
        feed.request();
        assert!(
            t0.elapsed() < Duration::from_millis(50),
            "spawn + request took {:?}",
            t0.elapsed()
        );
        let t1 = Instant::now();
        drop(feed);
        assert!(t1.elapsed() < Duration::from_millis(50), "drop must not join");
        drop(rx);
    }

    #[test]
    fn publishes_the_projects_snapshot() {
        let connected = Arc::new(AtomicBool::new(true));
        let (_feed, mut rx) =
            ProjectFeed::spawn_with("demo", canned("a"), sources(&connected), fast());
        let FeedEvent::Snapshot(snap) = next_matching(&mut rx, is_snapshot) else {
            unreachable!()
        };
        assert_eq!(snap.sidebar.unwrap().project_label, "demo:a");
    }

    #[test]
    fn a_lost_daemon_degrades_to_polling_and_recovers_on_its_own() {
        let connected = Arc::new(AtomicBool::new(true));
        let (_feed, mut rx) =
            ProjectFeed::spawn_with("demo", canned("a"), sources(&connected), fast());
        assert!(matches!(
            next_matching(&mut rx, |e| matches!(e, FeedEvent::Link(_))),
            FeedEvent::Link(DaemonLink::Live)
        ));
        next_matching(&mut rx, is_snapshot);

        // The daemon goes away: the indicator flips to polling, and
        // snapshots keep arriving with no wake at all.
        connected.store(false, Ordering::Release);
        assert!(matches!(
            next_matching(&mut rx, |e| matches!(e, FeedEvent::Link(_))),
            FeedEvent::Link(DaemonLink::Polling)
        ));
        for _ in 0..3 {
            next_matching(&mut rx, is_snapshot);
        }

        // It comes back: live again, without anyone asking.
        connected.store(true, Ordering::Release);
        assert!(matches!(
            next_matching(&mut rx, |e| matches!(e, FeedEvent::Link(_))),
            FeedEvent::Link(DaemonLink::Live)
        ));
    }

    #[test]
    fn a_daemon_change_wakes_a_refresh() {
        let connected = Arc::new(AtomicBool::new(true));
        let (change_tx, change_rx) = mpsc::channel();
        let (_feed, mut rx) = ProjectFeed::spawn_with(
            "demo",
            canned("a"),
            FeedSources {
                connected,
                changes: Some(change_rx),
                keep_alive: None,
            },
            fast(),
        );
        next_matching(&mut rx, is_snapshot);
        // Live with a 60s safety interval: only the wake can cause this one.
        change_tx.send(shelbi_app::changes::ChangeWake::Board).unwrap();
        next_matching(&mut rx, is_snapshot);
    }

    #[test]
    fn session_title_and_exit_reach_the_window() {
        let connected = Arc::new(AtomicBool::new(true));
        let (feed, mut rx) =
            ProjectFeed::spawn_with("demo", canned("a"), sources(&connected), fast());
        let sink = feed.session_sink();
        sink.forward("orch", &SessionEvent::Title("claude".into()));
        sink.forward("orch", &SessionEvent::Bell);
        let ev = next_matching(&mut rx, |e| matches!(e, FeedEvent::Session(_)));
        let FeedEvent::Session(notice) = ev else { unreachable!() };
        assert_eq!(
            notice,
            SessionNotice {
                session: "orch".into(),
                kind: SessionNoticeKind::Title("claude".into()),
            }
        );
    }

    #[test]
    fn the_feed_ends_when_the_window_drops_it() {
        let connected = Arc::new(AtomicBool::new(true));
        let (feed, rx) = ProjectFeed::spawn_with("demo", canned("a"), sources(&connected), fast());
        drop(feed);
        // The channel closes once the thread has wound down.
        let rest: Vec<FeedEvent> = futures::executor::block_on(rx.collect());
        assert!(rest.len() < 10);
    }

    #[test]
    fn control_watcher_wakes_only_the_named_project() {
        let watcher = ControlWatcher {
            wakes: Arc::new(Mutex::new(HashMap::new())),
        };
        let connected = Arc::new(AtomicBool::new(true));
        let (demo, mut demo_rx) =
            ProjectFeed::spawn_with("demo", canned("a"), sources(&connected), fast());
        watcher.watch("demo", &demo);
        next_matching(&mut demo_rx, is_snapshot);
        watcher.wake("other");
        watcher.wake("demo");
        next_matching(&mut demo_rx, is_snapshot);
        // A closed feed is pruned on the next wake.
        drop(demo);
        drop(demo_rx);
        thread::sleep(Duration::from_millis(50));
        watcher.wake("demo");
        assert!(watcher.wakes.lock().unwrap()["demo"].is_empty());
    }
}
