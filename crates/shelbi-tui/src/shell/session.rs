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
use std::time::{Duration, Instant};

use shelbi_client::{ClientError, Connection, SessionEvents};
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

/// Identity for the idle-workspace placeholder: a declared workspace the client
/// opened that has no live session (and no exited one to show a last line for).
/// The main area shows this instead of a bare "couldn't attach" error so the
/// sidebar selection and the main view agree (`rt-tui-idle-workspace-open`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleInfo {
    /// The workspace name.
    pub name: String,
    /// The machine the workspace is declared on.
    pub machine: String,
    /// The worktree's current branch, when known (local workspaces only).
    pub branch: Option<String>,
}

/// Why a connect didn't yield a live session.
pub enum ConnectFailure {
    /// A declared workspace with no session at all — the main area shows the
    /// idle-workspace placeholder rather than an error.
    Idle(IdleInfo),
    /// The session was just launched (its `meta.json` is on disk and discovery
    /// reports it alive) but its socket isn't accepting connections yet —
    /// `Connection::open` returned ENOENT / ECONNREFUSED. This is *transient*:
    /// the connect worker retries it with a short backoff rather than surfacing
    /// an error, because the session's `sock` is bound a beat after its dir
    /// appears (`rt-tui-attach-retry-unbound-socket`). The string is the error
    /// shown only if the retries run out.
    Starting(String),
    /// No live session exists yet, but one is *expected* to (re)appear, so this
    /// is transient like [`Starting`]: the connect worker keeps retrying on the
    /// `RetryPolicy` cadence and attaches as soon as a live session shows up.
    /// Raised for the orchestrator when the project declares one (its config
    /// loads) or an exited session with its name is already on disk — the shape
    /// of a relaunch in progress: a daemon restart SIGTERMs the old
    /// `<project>/orch` session and spawns a fresh one a beat later, and a TUI
    /// that connects in that gap would otherwise settle on a permanent
    /// "couldn't attach" error even though a healthy session arrives seconds
    /// later (`rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched`).
    /// The string is the terminal error shown only if no live session appears
    /// before the deadline (an exited session's last output line, when any).
    Awaiting(String),
    /// Any other no-session / attach failure. The message is shown as-is and,
    /// for a session that *exited*, already carries its last output line (so a
    /// dead workspace or orchestrator surfaces why it's gone — the behavior the
    /// orchestrator view already had).
    Message(String),
}

/// How the connect worker retries a session whose socket isn't up yet
/// ([`ConnectFailure::Starting`]). A launched session binds its `sock` a beat
/// after its dir appears, so the worker re-attempts the connect on this cadence
/// until the socket is up or the deadline passes
/// (`rt-tui-attach-retry-unbound-socket`).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total time to keep retrying a still-starting session before giving up and
    /// surfacing the error.
    pub deadline: Duration,
    /// Pause between attempts.
    pub backoff: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(15),
            backoff: Duration::from_millis(250),
        }
    }
}

/// The blocking work of connecting to a session, injectable so the event loop
/// can be tested against a connector that blocks without a real session.
pub trait Connector: Send + Sync {
    fn connect(&self, project: &str, target: &SessionRef) -> Result<Connected, ConnectFailure>;
}

/// The production connector: discover the session on disk, open it, and attach
/// with a full replay.
pub struct LiveConnector;

impl Connector for LiveConnector {
    fn connect(&self, project: &str, target: &SessionRef) -> Result<Connected, ConnectFailure> {
        let root = shelbi_state::sessions_dir().map_err(|e| ConnectFailure::Message(e.to_string()))?;
        let want = discovery_name(project, target);
        let sessions =
            shelbi_client::list(&root).map_err(|e| ConnectFailure::Message(e.to_string()))?;
        let sess = match sessions.iter().find(|s| s.meta.name == want && s.alive) {
            Some(s) => s.clone(),
            None => {
                // No live session. If a *dead* one with this name left a
                // `final.txt`, surface its last output line so the TUI explains
                // *why* the session isn't there — e.g. an orchestrator that
                // exited at launch with `command not found: claude` — instead of
                // only "no live session" (`rt-login-env-capture-empty-path`).
                let exited = sessions.iter().any(|s| s.meta.name == want && !s.alive);
                let last_line = sessions
                    .iter()
                    .find(|s| s.meta.name == want && !s.alive)
                    .and_then(|s| last_final_line(&s.dir));
                // A declared workspace with no exited session is *idle*, not
                // failed: show its identity + how to start, not a terse error
                // (`rt-tui-idle-workspace-open`). A session that exited (or any
                // non-workspace target) keeps the explanatory message.
                if let (SessionRef::Workspace(name), None) = (target, &last_line) {
                    return Err(ConnectFailure::Idle(idle_info(project, name)));
                }
                let msg = no_live_session_error(&want, last_line.as_deref());
                // The orchestrator is relaunched out of band — a daemon restart
                // SIGTERMs the old session and spawns a fresh one a beat later.
                // If the TUI connected in that gap it finds only the exited
                // session; rather than surfacing a terminal error the user must
                // quit past, keep retrying until the new session binds
                // (`rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched`).
                // `orchestrator_declared` is computed lazily, only when it could
                // change the outcome (an exited session already proves one is
                // expected), to avoid a config load on every workspace miss.
                if should_await_session(target, exited, || orchestrator_declared(project)) {
                    return Err(ConnectFailure::Awaiting(msg));
                }
                return Err(ConnectFailure::Message(msg));
            }
        };
        let (conn, events) = match Connection::open(&sess.sock, None, capability::ALL) {
            Ok(ce) => ce,
            // The session dir (with `meta.json`) appears before its `sock` is
            // bound, so a connect in that window fails with ENOENT (socket file
            // absent) or ECONNREFUSED (file there, not yet listening). Discovery
            // already told us the session is alive, so this is "still starting",
            // not a failure — signal a retry (`rt-tui-attach-retry-unbound-socket`).
            Err(ClientError::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Err(ConnectFailure::Starting(starting_error(&want)));
            }
            Err(e) => return Err(ConnectFailure::Message(e.to_string())),
        };
        let size = match conn.info() {
            Ok(info) => Size::new(info.cols.max(1), info.rows.max(1)),
            Err(_) => Size::new(80, 24),
        };
        conn.attach(None)
            .map_err(|e| ConnectFailure::Message(e.to_string()))?;
        Ok(Connected {
            conn,
            events,
            size,
        })
    }
}

/// Whether a missing live session for `target` should be *awaited* — kept
/// retrying by the connect worker — rather than reported as a terminal failure.
/// True only for the orchestrator, and only when a session is expected: an
/// exited one with its name is already on disk (`exited`), or the project
/// declares an orchestrator (`declared`). A relaunch in that window brings a
/// live session up within the retry deadline, so the TUI never settles on a
/// stale "couldn't attach" error across an orchestrator relaunch
/// (`rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched`).
///
/// `declared` is a closure so the (blocking) config load runs only when the
/// cheap `exited` signal didn't already settle it. Pure given its inputs, so
/// the classification is unit-testable without a sessions directory.
fn should_await_session(
    target: &SessionRef,
    exited: bool,
    declared: impl FnOnce() -> bool,
) -> bool {
    matches!(target, SessionRef::Orchestrator) && (exited || declared())
}

/// Whether `project` declares an orchestrator — i.e. its config loads. A
/// declared orchestrator is expected to have (or soon have) a live session.
/// Best-effort: a project whose config can't be read reads as "not declared"
/// and the connect falls back to the exited-session signal.
fn orchestrator_declared(project: &str) -> bool {
    shelbi_state::load_project(project).is_ok()
}

/// Assemble an idle workspace's [`IdleInfo`] from the project config and (for a
/// local machine) its worktree branch. Falls back to just the name when the
/// config can't be read, so the placeholder always has something to show.
fn idle_info(project: &str, name: &str) -> IdleInfo {
    match shelbi_orchestrator::workspace::resolve_idle_workspace(project, name) {
        Some(id) => IdleInfo {
            name: name.to_string(),
            machine: id.machine,
            branch: id.branch,
        },
        None => IdleInfo {
            name: name.to_string(),
            machine: String::new(),
            branch: None,
        },
    }
}

/// Compose the error shown when no live session named `want` exists, appending a
/// dead session's last output line when one is available. Split out (pure) so the
/// message is unit-testable without a real sessions directory.
fn no_live_session_error(want: &str, last_line: Option<&str>) -> String {
    match last_line {
        Some(line) if !line.is_empty() => {
            format!("no live session `{want}` — last output: {line}")
        }
        _ => format!("no live session `{want}`"),
    }
}

/// The error shown when a just-launched session's socket never comes up within
/// the retry deadline. Pure, so it's unit-testable without a real session.
fn starting_error(want: &str) -> String {
    format!("session `{want}` is starting (socket not up yet)")
}

/// The last non-empty line of a dead session's `<dir>/final.txt`, trimmed, or
/// `None` when the file is absent or blank. This is the exited session's final
/// screen line (e.g. `zsh:1: command not found: claude`).
fn last_final_line(dir: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("final.txt")).ok()?;
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_string)
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
        rx: Receiver<Result<Connected, ConnectFailure>>,
        _join: JoinHandle<()>,
    },
    Live(Box<LiveSlot>),
    /// A declared workspace with no live session — the main area shows the
    /// idle-workspace placeholder (`rt-tui-idle-workspace-open`).
    Idle {
        target: SessionRef,
        info: IdleInfo,
    },
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
    /// How the connect worker retries a session whose socket isn't up yet.
    retry: RetryPolicy,
}

/// What the main area should draw right now.
pub enum MainState<'a> {
    Empty,
    Connecting(&'a SessionRef),
    Live(&'a TerminalPane),
    /// An idle workspace: render its placeholder (`rt-tui-idle-workspace-open`).
    Idle(&'a IdleInfo),
    Failed(&'a SessionRef, &'a str),
}

impl SessionManager {
    pub fn new(project: impl Into<String>, connector: std::sync::Arc<dyn Connector>) -> Self {
        Self {
            project: project.into(),
            connector,
            slot: Slot::Empty,
            retry: RetryPolicy::default(),
        }
    }

    /// Begin showing `target`. The blocking connect runs on a worker thread;
    /// this returns at once.
    ///
    /// Re-selecting the same target no-ops only while it is actively connecting
    /// or already live — there is nothing to redo there. A *failed* (or idle)
    /// binding instead re-attempts, so re-selecting Chat in the sidebar recovers
    /// from a missed relaunch without a full quit/reopen
    /// (`rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched`).
    pub fn show(&mut self, target: SessionRef) {
        if self.current_target() == Some(&target)
            && matches!(self.slot, Slot::Connecting { .. } | Slot::Live(_))
        {
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
            retry: self.retry,
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
                Err(TryRecvError::Disconnected) => Err(ConnectFailure::Message(
                    "connect worker stopped unexpectedly".to_string(),
                )),
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
            Err(ConnectFailure::Idle(info)) => Slot::Idle { target, info },
            // The worker only ever forwards a terminal outcome: it retries the
            // transient `Starting`/`Awaiting` signals itself and, on give-up,
            // converts them to a `Message`. One arriving here would mean the
            // worker didn't retry, so treat it as a plain failure rather than
            // silently dropping it.
            Err(
                ConnectFailure::Starting(error)
                | ConnectFailure::Awaiting(error)
                | ConnectFailure::Message(error),
            ) => Slot::Failed { target, error },
        };
        true
    }

    /// Retry the current binding when it settled on a non-live outcome (idle or
    /// failed). Used when a board/workspace change arrives while the main area
    /// shows an idle/failed workspace, so a session that just started attaches
    /// without the user re-selecting the row (`rt-tui-idle-workspace-open`).
    /// A no-op while connecting, live, or empty — returns whether it kicked a
    /// reconnect.
    pub fn retry_if_stale(&mut self) -> bool {
        if matches!(self.slot, Slot::Idle { .. } | Slot::Failed { .. }) {
            self.reconnect();
            true
        } else {
            false
        }
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
            Slot::Connecting { target, .. }
            | Slot::Idle { target, .. }
            | Slot::Failed { target, .. } => Some(target),
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
            Slot::Idle { info, .. } => MainState::Idle(info),
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
    tx: mpsc::Sender<Result<Connected, ConnectFailure>>,
    retry: RetryPolicy,
}

impl ConnectJob {
    fn run(self) {
        // Retry a session whose socket isn't up yet. A just-launched session has
        // its dir (with `meta.json`) on disk a beat before its `sock` is bound,
        // so a connect in that window returns `Starting` — we back off and try
        // again rather than surfacing an error, until the socket is up or the
        // deadline passes (`rt-tui-attach-retry-unbound-socket`). Every other
        // outcome (a live attach, an idle workspace, an exited session) is
        // terminal and forwarded at once. Running the wait here, on the worker
        // thread, keeps the UI loop non-blocking — it only ever polls.
        let started = Instant::now();
        let mut result = self.connector.connect(&self.project, &self.target);
        while matches!(
            &result,
            Err(ConnectFailure::Starting(_) | ConnectFailure::Awaiting(_))
        ) {
            if started.elapsed() >= self.retry.deadline {
                // Give up: turn the transient signal into a terminal error so
                // the user finally sees why the attach never completed.
                result = match result {
                    // A socket that never bound — name the give-up explicitly,
                    // since the carried message only says "socket not up yet".
                    Err(ConnectFailure::Starting(msg)) => Err(ConnectFailure::Message(format!(
                        "{msg} — not reachable after {:?}",
                        self.retry.deadline
                    ))),
                    // A session that never (re)appeared — surface its carried
                    // message as-is: the exited session's last output line, when
                    // any, which already explains why nothing is there.
                    Err(ConnectFailure::Awaiting(msg)) => Err(ConnectFailure::Message(msg)),
                    other => other,
                };
                break;
            }
            std::thread::sleep(self.retry.backoff);
            result = self.connector.connect(&self.project, &self.target);
        }
        // The UI may have moved on; a failed send just means nobody is waiting.
        let _ = self.tx.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;

    fn connecting(m: &SessionManager) -> bool {
        matches!(m.state(), MainState::Connecting(_))
    }

    #[test]
    fn no_live_session_error_appends_the_exited_sessions_last_line() {
        // An orchestrator that exited at launch has its final screen line
        // surfaced, so the TUI shows the cause instead of only "no live session".
        let msg = no_live_session_error(
            "contextstore/orch",
            Some("zsh:1: command not found: claude"),
        );
        assert_eq!(
            msg,
            "no live session `contextstore/orch` — last output: zsh:1: command not found: claude"
        );
    }

    #[test]
    fn no_live_session_error_without_a_last_line_is_the_bare_message() {
        assert_eq!(
            no_live_session_error("demo/orch", None),
            "no live session `demo/orch`"
        );
        // An empty last line is treated as absent.
        assert_eq!(
            no_live_session_error("demo/orch", Some("")),
            "no live session `demo/orch`"
        );
    }

    #[test]
    fn last_final_line_reads_the_last_nonblank_line_of_final_txt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("final.txt"),
            "booting orchestrator\nzsh:1: command not found: claude\n\n   \n",
        )
        .unwrap();
        assert_eq!(
            last_final_line(dir.path()).as_deref(),
            Some("zsh:1: command not found: claude")
        );
    }

    #[test]
    fn last_final_line_is_none_without_final_txt() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(last_final_line(dir.path()), None);
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
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            // Block until the test drops its sender.
            let rx = self.release.lock().unwrap().take();
            if let Some(rx) = rx {
                let _ = rx.recv();
            }
            Err(ConnectFailure::Message("released".into()))
        }
    }

    /// A connector that always reports the target as an idle workspace.
    struct IdleConnector(IdleInfo);

    impl Connector for IdleConnector {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            Err(ConnectFailure::Idle(self.0.clone()))
        }
    }

    /// Drive `mgr.poll()` until it reports the connect settled (or the budget
    /// runs out), so a test can assert the resolved [`MainState`].
    fn poll_until_settled(mgr: &mut SessionManager) {
        for _ in 0..400 {
            if mgr.poll() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the connect never settled");
    }

    #[test]
    fn an_idle_workspace_lands_in_the_idle_state() {
        let info = IdleInfo {
            name: "vector".into(),
            machine: "hub".into(),
            branch: Some("main".into()),
        };
        let mut mgr = SessionManager::new("demo", std::sync::Arc::new(IdleConnector(info)));
        mgr.show(SessionRef::Workspace("vector".into()));
        poll_until_settled(&mut mgr);
        match mgr.state() {
            MainState::Idle(got) => {
                assert_eq!(got.name, "vector");
                assert_eq!(got.machine, "hub");
                assert_eq!(got.branch.as_deref(), Some("main"));
            }
            _ => panic!("an idle workspace resolves to MainState::Idle"),
        }
        // The target is still bound, so a retry knows what to reconnect to.
        assert_eq!(
            mgr.current_target(),
            Some(&SessionRef::Workspace("vector".into()))
        );
    }

    #[test]
    fn retry_if_stale_reconnects_an_idle_slot_but_not_a_connecting_one() {
        let info = IdleInfo {
            name: "vector".into(),
            machine: "hub".into(),
            branch: None,
        };
        let mut mgr = SessionManager::new("demo", std::sync::Arc::new(IdleConnector(info)));
        mgr.show(SessionRef::Workspace("vector".into()));
        poll_until_settled(&mut mgr);
        assert!(matches!(mgr.state(), MainState::Idle(_)));

        // A change notification retries the idle slot: it goes back to
        // connecting (the worker re-runs) without the caller re-selecting.
        assert!(mgr.retry_if_stale(), "an idle slot retries");
        assert!(matches!(mgr.state(), MainState::Connecting(_)));

        // While connecting, a further retry is a no-op (nothing stacks).
        assert!(!mgr.retry_if_stale(), "a connecting slot does not retry");
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

    /// A connector that reports the session as still-starting (socket not up) for
    /// the first `starting` calls, then opens a real session socket and returns a
    /// live binding. Counts its calls so a test can assert the worker retried.
    struct FlakyThenLive {
        sock: std::path::PathBuf,
        starting: usize,
        calls: AtomicUsize,
    }

    impl Connector for FlakyThenLive {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.starting {
                return Err(ConnectFailure::Starting("socket not up yet".into()));
            }
            let (conn, events) = Connection::open(&self.sock, None, capability::ALL)
                .map_err(|e| ConnectFailure::Message(e.to_string()))?;
            let size = match conn.info() {
                Ok(info) => Size::new(info.cols.max(1), info.rows.max(1)),
                Err(_) => Size::new(80, 24),
            };
            conn.attach(None)
                .map_err(|e| ConnectFailure::Message(e.to_string()))?;
            Ok(Connected { conn, events, size })
        }
    }

    /// A connector that reports the session as expected-but-not-live
    /// (`Awaiting`) for the first `awaiting` calls — the shape of an
    /// orchestrator relaunch where the TUI finds only the exited session on disk
    /// — then opens a real socket and returns a live binding. Counts its calls
    /// so a test can assert the worker retried.
    struct AwaitingThenLive {
        sock: std::path::PathBuf,
        awaiting: usize,
        calls: AtomicUsize,
    }

    impl Connector for AwaitingThenLive {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.awaiting {
                return Err(ConnectFailure::Awaiting("no live session `relaunch/orch`".into()));
            }
            let (conn, events) = Connection::open(&self.sock, None, capability::ALL)
                .map_err(|e| ConnectFailure::Message(e.to_string()))?;
            let size = match conn.info() {
                Ok(info) => Size::new(info.cols.max(1), info.rows.max(1)),
                Err(_) => Size::new(80, 24),
            };
            conn.attach(None)
                .map_err(|e| ConnectFailure::Message(e.to_string()))?;
            Ok(Connected { conn, events, size })
        }
    }

    /// A connector that always reports the session as still-starting, so the
    /// socket never comes up: the worker retries until the deadline, then fails.
    struct AlwaysStarting {
        calls: AtomicUsize,
    }

    impl Connector for AlwaysStarting {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(ConnectFailure::Starting("session `x` is starting".into()))
        }
    }

    /// A connector for a session that *exited*: a terminal message carrying the
    /// dead session's last output line. Not retryable.
    struct ExitedConnector;

    impl Connector for ExitedConnector {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            Err(ConnectFailure::Message(
                "no live session `demo/orch` — last output: command not found: claude".into(),
            ))
        }
    }

    /// Spawn a genuine `shelbi_session::run` (a raw, no-echo `cat` child) on a
    /// thread, returning the bound socket and a guard that kills it on drop. The
    /// caller must hold `ENV_LOCK` (this sets the process-global `SHELBI_HOME`).
    /// Mirrors the real-session scaffolding in `pty_input_tests`.
    fn spawn_cat_session(home: &std::path::Path, name: &str) -> (std::path::PathBuf, CatCleanup) {
        use shelbi_session::layout::SessionPaths;
        use shelbi_session::RunArgs;
        std::env::set_var("SHELBI_HOME", home);
        let id = shelbi_session::layout::derive_id_now(name);
        let paths = SessionPaths::new(&home.join("sessions"), &id);
        let args = RunArgs {
            id,
            name: name.to_string(),
            cwd: std::env::temp_dir(),
            cols: 80,
            rows: 24,
            task: None,
            raw_output_log: false,
            child_argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "stty raw -echo 2>/dev/null; exec cat".into(),
            ],
            manage_daemon: false,
        };
        std::thread::spawn(move || shelbi_session::run(args));
        let sock = paths.sock();
        for _ in 0..500 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sock.exists(), "the session socket should appear");
        (sock.clone(), CatCleanup { sock })
    }

    struct CatCleanup {
        sock: std::path::PathBuf,
    }

    impl Drop for CatCleanup {
        fn drop(&mut self) {
            if let Ok((conn, _)) = Connection::open(&self.sock, None, capability::ALL) {
                let _ = conn.kill(Some(libc::SIGKILL));
            }
            std::env::remove_var("SHELBI_HOME");
        }
    }

    /// Poll the manager until it reaches `MainState::Live`, or the budget runs
    /// out. Returns whether it went live.
    fn poll_until_live(mgr: &mut SessionManager) -> bool {
        for _ in 0..500 {
            mgr.poll();
            if matches!(mgr.state(), MainState::Live(_)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_still_starting_session_retries_then_attaches_live() {
        // AC1: a connector that reports "socket not up" for the first few attempts
        // and then succeeds ends in a live attach, with no error ever shown.
        let _lock = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let (sock, _cleanup) = spawn_cat_session(home.path(), "retry/orch");

        let connector = std::sync::Arc::new(FlakyThenLive {
            sock,
            starting: 3,
            calls: AtomicUsize::new(0),
        });
        let mut mgr = SessionManager::new("retry", connector.clone());
        // A short backoff so the three "starting" attempts clear quickly; a
        // generous deadline so the real attach has room.
        mgr.retry = RetryPolicy {
            deadline: Duration::from_secs(5),
            backoff: Duration::from_millis(10),
        };
        mgr.show(SessionRef::Orchestrator);

        assert!(
            poll_until_live(&mut mgr),
            "the retried connect should end in a live attach"
        );
        assert!(
            matches!(mgr.state(), MainState::Live(_)),
            "no error is shown — the slot is live"
        );
        // The worker retried past the three still-starting attempts.
        assert!(
            connector.calls.load(Ordering::SeqCst) >= 4,
            "the worker should retry the still-starting attempts, not fail on the first"
        );
    }

    #[test]
    fn a_socket_that_never_appears_fails_after_the_bounded_wait() {
        // AC2: a session whose socket never comes up retries until the deadline,
        // then surfaces the error — and only then.
        let connector = std::sync::Arc::new(AlwaysStarting {
            calls: AtomicUsize::new(0),
        });
        let mut mgr = SessionManager::new("demo", connector.clone());
        let deadline = Duration::from_millis(120);
        mgr.retry = RetryPolicy {
            deadline,
            backoff: Duration::from_millis(10),
        };

        let start = Instant::now();
        mgr.show(SessionRef::Orchestrator);
        poll_until_settled(&mut mgr);
        let elapsed = start.elapsed();

        // It did not give up before the bounded wait elapsed.
        assert!(
            elapsed >= Duration::from_millis(100),
            "the error should appear only after the bounded wait (took {elapsed:?})"
        );
        // It retried rather than failing on the first attempt.
        assert!(
            connector.calls.load(Ordering::SeqCst) > 1,
            "a still-starting session should be retried"
        );
        match mgr.state() {
            MainState::Failed(_, err) => {
                assert!(err.contains("starting"), "carries the starting message: {err}");
                assert!(err.contains("not reachable"), "names the give-up: {err}");
            }
            _ => panic!("a socket that never appears lands in MainState::Failed"),
        }
    }

    #[test]
    fn an_exited_session_shows_its_last_line_immediately() {
        // AC3: an exited session's terminal message (with its last output line) is
        // shown at once — the retry deadline is never waited on.
        let mut mgr = SessionManager::new("demo", std::sync::Arc::new(ExitedConnector));
        // A long deadline: if a terminal failure were (wrongly) retried, the test
        // would stall well past this budget.
        mgr.retry = RetryPolicy {
            deadline: Duration::from_secs(30),
            backoff: Duration::from_millis(10),
        };

        let start = Instant::now();
        mgr.show(SessionRef::Orchestrator);
        poll_until_settled(&mut mgr);
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "an exited session fails immediately, without waiting the deadline (took {elapsed:?})"
        );
        match mgr.state() {
            MainState::Failed(_, err) => assert_eq!(
                err,
                "no live session `demo/orch` — last output: command not found: claude"
            ),
            _ => panic!("an exited session lands in MainState::Failed with its last line"),
        }
    }

    #[test]
    fn starting_error_reads_as_a_terminal_message() {
        assert_eq!(
            starting_error("demo/orch"),
            "session `demo/orch` is starting (socket not up yet)"
        );
    }

    #[test]
    fn an_awaiting_orchestrator_retries_then_attaches_live() {
        // AC4: the connector sees only an exited session first (`Awaiting`) and
        // then a live one — the orchestrator relaunched as the TUI opened — and
        // ends up attached, with no error ever shown.
        let _lock = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let (sock, _cleanup) = spawn_cat_session(home.path(), "relaunch/orch");

        let connector = std::sync::Arc::new(AwaitingThenLive {
            sock,
            awaiting: 3,
            calls: AtomicUsize::new(0),
        });
        let mut mgr = SessionManager::new("relaunch", connector.clone());
        mgr.retry = RetryPolicy {
            deadline: Duration::from_secs(5),
            backoff: Duration::from_millis(10),
        };
        mgr.show(SessionRef::Orchestrator);

        assert!(
            poll_until_live(&mut mgr),
            "the awaited connect should end in a live attach once the new session binds"
        );
        assert!(
            matches!(mgr.state(), MainState::Live(_)),
            "no error is shown across the relaunch — the slot is live"
        );
        assert!(
            connector.calls.load(Ordering::SeqCst) >= 4,
            "the worker should keep retrying past the awaiting attempts, not fail on the first"
        );
    }

    #[test]
    fn an_awaiting_session_that_never_appears_shows_its_carried_message() {
        // On give-up, an `Awaiting` outcome surfaces its carried message as-is
        // (the exited session's last output line) — not the `Starting` "not
        // reachable" phrasing, which is about a socket that never bound.
        struct AlwaysAwaiting;
        impl Connector for AlwaysAwaiting {
            fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
                Err(ConnectFailure::Awaiting(
                    "no live session `demo/orch` — last output: goodbye".into(),
                ))
            }
        }
        let mut mgr = SessionManager::new("demo", std::sync::Arc::new(AlwaysAwaiting));
        mgr.retry = RetryPolicy {
            deadline: Duration::from_millis(60),
            backoff: Duration::from_millis(10),
        };
        mgr.show(SessionRef::Orchestrator);
        poll_until_settled(&mut mgr);
        match mgr.state() {
            MainState::Failed(_, err) => assert_eq!(
                err, "no live session `demo/orch` — last output: goodbye",
                "the carried message is surfaced verbatim, with no give-up suffix"
            ),
            _ => panic!("an awaited session that never appears lands in MainState::Failed"),
        }
    }

    #[test]
    fn should_await_session_only_awaits_an_expected_orchestrator() {
        // The orchestrator is awaited when an exited session is present or one is
        // declared; with neither signal it is a terminal failure.
        assert!(should_await_session(&SessionRef::Orchestrator, true, || false));
        assert!(should_await_session(&SessionRef::Orchestrator, false, || true));
        assert!(!should_await_session(&SessionRef::Orchestrator, false, || false));
        // Workspaces and review content are never awaited — they keep their
        // idle / terminal-message behavior.
        assert!(!should_await_session(
            &SessionRef::Workspace("alpha".into()),
            true,
            || true
        ));
        assert!(!should_await_session(
            &SessionRef::Review {
                slot: "r".into(),
                role: "editor".into()
            },
            true,
            || true
        ));
    }

    #[test]
    fn should_await_session_skips_the_declared_check_when_exited_settles_it() {
        // The `declared` closure (a blocking config load) must not run when the
        // cheap exited-session signal already proves a session is expected.
        let called = std::cell::Cell::new(false);
        assert!(should_await_session(&SessionRef::Orchestrator, true, || {
            called.set(true);
            true
        }));
        assert!(!called.get(), "the declared check is skipped when exited is true");
    }

    #[test]
    fn re_showing_a_failed_target_re_attempts() {
        // AC3: a failed main view re-attempts when the user re-selects it (e.g.
        // Chat in the sidebar), so a missed relaunch never needs a quit/reopen.
        let mut mgr = SessionManager::new("demo", std::sync::Arc::new(ExitedConnector));
        mgr.retry = RetryPolicy {
            deadline: Duration::from_millis(60),
            backoff: Duration::from_millis(10),
        };
        mgr.show(SessionRef::Orchestrator);
        poll_until_settled(&mut mgr);
        assert!(
            matches!(mgr.state(), MainState::Failed(..)),
            "the exited session first settles as a failure"
        );
        // Re-selecting the same (failed) target kicks a fresh connect rather than
        // no-opping, so the user can recover without a quit/reopen.
        mgr.show(SessionRef::Orchestrator);
        assert!(
            connecting(&mgr),
            "re-showing a failed target re-attempts instead of staying failed"
        );
    }
}
