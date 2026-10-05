//! Binding the shell's main area to a live session, with every blocking step
//! off the UI thread.
//!
//! In one process a blocked call freezes every view, so connecting to a session
//! — which does socket I/O, an `info` round-trip, and a replay attach — runs on
//! a worker thread and reports back over a channel. The UI loop only ever
//! *polls*; it never blocks on a connect. Scrollback is held for the one
//! session being viewed, so switching away drops the previous connection (plan,
//! "The TUI becomes one process": "Scrollback is held only for sessions being
//! viewed").

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;

use shelbi_client::{Connection, SessionEvents};
use shelbi_proto::capability;
use shelbi_term::Size;

use super::terminal_view::TerminalPane;

/// Which session a sidebar row refers to. The concrete session *name* used for
/// discovery is derived from the project (sessions are named `<project>/orch`
/// and `<project>/ws/<workspace>` by the session backend).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRef {
    /// The project's orchestrator chat session.
    Orchestrator,
    /// A dev workspace's session.
    Workspace(String),
    /// A review slot's content session: the editor or diff tool the review
    /// interface shows in a terminal view (`rt-tui-review`). `slot` is the
    /// review workspace, `role` is `editor` / `diff`. The daemon owns their
    /// lifetime (see [`shelbi_orchestrator::review_session`]); the shell only
    /// attaches. The review slot's agent (chat) session is a
    /// [`SessionRef::Workspace`] of the slot name.
    Review { slot: String, role: String },
}

impl SessionRef {
    /// A human label for the status line / title.
    pub fn display(&self) -> String {
        match self {
            SessionRef::Orchestrator => "orchestrator".to_string(),
            SessionRef::Workspace(w) => w.clone(),
            SessionRef::Review { slot, role } => format!("{slot} {role}"),
        }
    }
}

/// The session backend's discovery name for a ref within `project`.
pub fn discovery_name(project: &str, r: &SessionRef) -> String {
    match r {
        SessionRef::Orchestrator => format!("{project}/orch"),
        SessionRef::Workspace(w) => format!("{project}/ws/{w}"),
        SessionRef::Review { slot, role } => format!("{project}/review/{slot}/{role}"),
    }
}

/// A freshly connected session handed back from the worker thread.
pub struct Connected {
    pub conn: Connection,
    pub events: SessionEvents,
    pub size: Size,
}

/// The blocking work of connecting to a session, injectable so the event loop
/// can be tested against a connector that blocks without a real session.
pub trait Connector: Send + Sync {
    fn connect(&self, project: &str, target: &SessionRef) -> Result<Connected, String>;
}

/// The production connector: discover the session on disk, open it, and attach
/// with a full replay.
pub struct LiveConnector;

impl Connector for LiveConnector {
    fn connect(&self, project: &str, target: &SessionRef) -> Result<Connected, String> {
        let root = shelbi_state::sessions_dir().map_err(|e| e.to_string())?;
        let want = discovery_name(project, target);
        let sessions = shelbi_client::list(&root).map_err(|e| e.to_string())?;
        let sess = sessions
            .into_iter()
            .find(|s| s.meta.name == want && s.alive)
            .ok_or_else(|| format!("no live session `{want}`"))?;
        let (conn, events) =
            Connection::open(&sess.sock, None, capability::ALL).map_err(|e| e.to_string())?;
        let size = match conn.info() {
            Ok(info) => Size::new(info.cols.max(1), info.rows.max(1)),
            Err(_) => Size::new(80, 24),
        };
        conn.attach(None).map_err(|e| e.to_string())?;
        Ok(Connected {
            conn,
            events,
            size,
        })
    }
}

/// A connected, live session binding. Boxed inside [`Slot`] because it is much
/// larger than the other variants (a `Connection`, its event stream, and a full
/// emulator pane).
struct LiveSlot {
    target: SessionRef,
    conn: Connection,
    events: SessionEvents,
    pane: TerminalPane,
}

/// The current main-area binding.
enum Slot {
    Empty,
    Connecting {
        target: SessionRef,
        rx: Receiver<Result<Connected, String>>,
        _join: JoinHandle<()>,
    },
    Live(Box<LiveSlot>),
    Failed {
        target: SessionRef,
        error: String,
    },
}

/// Owns the one session the main area is currently showing.
pub struct SessionManager {
    project: String,
    connector: std::sync::Arc<dyn Connector>,
    slot: Slot,
}

/// What the main area should draw right now.
pub enum MainState<'a> {
    Empty,
    Connecting(&'a SessionRef),
    Live(&'a TerminalPane),
    Failed(&'a SessionRef, &'a str),
}

impl SessionManager {
    pub fn new(project: impl Into<String>, connector: std::sync::Arc<dyn Connector>) -> Self {
        Self {
            project: project.into(),
            connector,
            slot: Slot::Empty,
        }
    }

    /// Begin showing `target`. No-op if it is already the live/connecting
    /// target. The blocking connect runs on a worker thread; this returns at
    /// once.
    pub fn show(&mut self, target: SessionRef) {
        if self.current_target() == Some(&target) {
            return;
        }
        let project = self.project.clone();
        let (tx, rx) = mpsc::channel();
        // The connector is shared, so hand the worker a raw pointer-free clone
        // of what it needs by moving a boxed job. We keep the connector on the
        // manager and run it through a trait object the worker borrows via an
        // Arc.
        let job = ConnectJob {
            connector: self.connector.clone(),
            project,
            target: target.clone(),
            tx,
        };
        let join = std::thread::Builder::new()
            .name("shelbi-shell-connect".into())
            .spawn(move || job.run())
            .expect("spawn connect worker");
        self.slot = Slot::Connecting {
            target,
            rx,
            _join: join,
        };
    }

    /// Poll the in-flight connect without blocking. Returns `true` when the
    /// slot changed (a connect finished or failed) so the caller redraws.
    pub fn poll(&mut self) -> bool {
        let result = match &self.slot {
            Slot::Connecting { rx, .. } => match rx.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => {
                    Err("connect worker stopped unexpectedly".to_string())
                }
            },
            _ => return false,
        };
        // Replace the Connecting slot with the outcome.
        let target = match std::mem::replace(&mut self.slot, Slot::Empty) {
            Slot::Connecting { target, .. } => target,
            other => {
                self.slot = other;
                return false;
            }
        };
        self.slot = match result {
            Ok(c) => Slot::Live(Box::new(LiveSlot {
                target,
                conn: c.conn,
                events: c.events,
                pane: TerminalPane::new(c.size),
            })),
            Err(error) => Slot::Failed { target, error },
        };
        true
    }

    /// Drain any pending output from the live session into its pane. Returns
    /// `true` if the screen changed (so the caller redraws), and rings the
    /// terminal bell if the session asked for one.
    pub fn pump_output(&mut self, ring_bell: &mut bool) -> bool {
        let Slot::Live(live) = &mut self.slot else {
            return false;
        };
        let mut changed = false;
        while let Some(ev) = live.events.try_recv() {
            changed |= live.pane.apply_event(ev);
        }
        if live.pane.bell {
            *ring_bell = true;
            live.pane.bell = false;
        }
        changed
    }

    pub fn current_target(&self) -> Option<&SessionRef> {
        match &self.slot {
            Slot::Empty => None,
            Slot::Connecting { target, .. } | Slot::Failed { target, .. } => Some(target),
            Slot::Live(live) => Some(&live.target),
        }
    }

    /// Re-attach to the current target with a fresh connect, even if it is
    /// already the connecting/failed/live target (which [`show`](Self::show)
    /// would no-op). Used after a background bootstrap brings the orchestrator
    /// session up, so an attach that failed because the session didn't exist yet
    /// retries (`rt-tui-headless-startup-block`). A no-op when nothing is bound.
    pub fn reconnect(&mut self) {
        if let Some(target) = self.current_target().cloned() {
            // Clear the slot first so `show` doesn't recognise the target as
            // already-current and skip the reconnect.
            self.slot = Slot::Empty;
            self.show(target);
        }
    }

    pub fn state(&self) -> MainState<'_> {
        match &self.slot {
            Slot::Empty => MainState::Empty,
            Slot::Connecting { target, .. } => MainState::Connecting(target),
            Slot::Live(live) => MainState::Live(&live.pane),
            Slot::Failed { target, error } => MainState::Failed(target, error),
        }
    }

    pub fn live_pane_mut(&mut self) -> Option<&mut TerminalPane> {
        match &mut self.slot {
            Slot::Live(live) => Some(&mut live.pane),
            _ => None,
        }
    }

    /// Send raw input bytes to the live session (best-effort).
    pub fn send_input(&self, bytes: &[u8]) {
        if let Slot::Live(live) = &self.slot {
            let _ = live.conn.input(bytes);
        }
    }

    /// Paste text into the live session (the session brackets it if it can).
    pub fn send_paste(&self, text: &str) {
        if let Slot::Live(live) = &self.slot {
            let _ = live.conn.paste(text);
        }
    }

    /// Report the main area's size to the live session so it reflows to fill
    /// it when the shell is the most-recently-active client.
    pub fn resize(&self, cols: u16, rows: u16) {
        if let Slot::Live(live) = &self.slot {
            let _ = live.conn.resize(cols.max(1), rows.max(1));
        }
    }
}

/// A connect job run on a worker thread.
struct ConnectJob {
    connector: std::sync::Arc<dyn Connector>,
    project: String,
    target: SessionRef,
    tx: mpsc::Sender<Result<Connected, String>>,
}

impl ConnectJob {
    fn run(self) {
        let result = self.connector.connect(&self.project, &self.target);
        // The UI may have moved on; a failed send just means nobody is waiting.
        let _ = self.tx.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Sender;
    use std::time::{Duration, Instant};

    fn connecting(m: &SessionManager) -> bool {
        matches!(m.state(), MainState::Connecting(_))
    }

    #[test]
    fn discovery_names_follow_the_backend_scheme() {
        assert_eq!(
            discovery_name("shelbi", &SessionRef::Orchestrator),
            "shelbi/orch"
        );
        assert_eq!(
            discovery_name("shelbi", &SessionRef::Workspace("alpha".into())),
            "shelbi/ws/alpha"
        );
    }

    /// A connector that blocks until the test releases it, so we can prove that
    /// a slow connect never stalls the UI thread.
    struct BlockingConnector {
        release: std::sync::Mutex<Option<Receiver<()>>>,
    }

    impl Connector for BlockingConnector {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, String> {
            // Block until the test drops its sender.
            let rx = self.release.lock().unwrap().take();
            if let Some(rx) = rx {
                let _ = rx.recv();
            }
            Err("released".into())
        }
    }

    #[test]
    fn a_blocked_connect_never_freezes_the_ui() {
        let (tx, rx): (Sender<()>, Receiver<()>) = mpsc::channel();
        let connector = BlockingConnector {
            release: std::sync::Mutex::new(Some(rx)),
        };
        let mut mgr = SessionManager::new("proj", std::sync::Arc::new(connector));

        // Issuing the connect returns immediately, even though the connector is
        // still blocked inside its worker thread.
        let start = Instant::now();
        mgr.show(SessionRef::Orchestrator);
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "show() must not block on the connect"
        );
        assert!(connecting(&mgr));

        // Polling is non-blocking and reports "nothing yet" while the worker is
        // still stuck.
        let start = Instant::now();
        for _ in 0..50 {
            assert!(!mgr.poll(), "nothing ready while the connector blocks");
        }
        assert!(
            start.elapsed() < Duration::from_millis(200),
            "poll() must never block"
        );
        assert!(connecting(&mgr), "still connecting");

        // Release the worker; the next poll observes the (failed) outcome.
        drop(tx);
        let mut saw_change = false;
        for _ in 0..200 {
            if mgr.poll() {
                saw_change = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_change, "the released connect eventually resolves");
        assert!(!connecting(&mgr));
        match mgr.state() {
            MainState::Failed(_, err) => assert_eq!(err, "released"),
            _ => panic!("a failed connect lands in the Failed state"),
        }
    }

    #[test]
    fn showing_the_same_target_twice_is_a_noop() {
        let (_tx, rx): (Sender<()>, Receiver<()>) = mpsc::channel();
        let connector = BlockingConnector {
            release: std::sync::Mutex::new(Some(rx)),
        };
        let mut mgr = SessionManager::new("proj", std::sync::Arc::new(connector));
        mgr.show(SessionRef::Orchestrator);
        assert!(connecting(&mgr));
        // A second request for the same target does not spawn another connect.
        mgr.show(SessionRef::Orchestrator);
        assert_eq!(mgr.current_target(), Some(&SessionRef::Orchestrator));
    }
}
