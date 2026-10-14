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
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use shelbi_app::session::{exit_line, DeathAction, RestartController};
use shelbi_client::{Connection, SessionEvents};
use shelbi_term::Size;

use super::terminal_view::TerminalPane;

// The attach / restart vocabulary (session naming, the relaunch-tolerant
// connect classification, the retry policy, the relaunch seam, the notices) is
// shared with the desktop app in `shelbi_app::session`; re-exported so the
// shell's modules keep their `session::` paths.
pub use shelbi_app::session::{
    gave_up_notice, ConnectFailure, Connected, Connector, IdleInfo, NoopRelauncher, Relauncher,
    RetryPolicy, SessionRef,
};

/// The production connector: discover the session on disk, open it, and attach
/// with a full replay ([`shelbi_app::session::connect`]), resolving an idle
/// workspace's placeholder identity from the project config.
pub struct LiveConnector;

impl Connector for LiveConnector {
    fn connect(&self, project: &str, target: &SessionRef) -> Result<Connected, ConnectFailure> {
        shelbi_app::session::connect(project, target, idle_info)
    }
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
        None => IdleInfo::named(name),
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
        rx: Receiver<Result<Connected, ConnectFailure>>,
        _join: JoinHandle<()>,
        /// The restart attempt this connect is serving, when it was kicked by
        /// the supervisor (1-based). `None` for an ordinary first connect, so
        /// the view shows "Connecting…" rather than "restarting (N/…)".
        restart_attempt: Option<u32>,
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
    /// A pane that exited unexpectedly and is waiting out its restart backoff
    /// before the next attempt. Rendered as "restarting" so the user sees the
    /// pane is coming back, not that it is gone.
    AwaitingRestart {
        target: SessionRef,
        /// The last output / exit line, carried so a later give-up can show it.
        last_line: String,
        /// The attempt that will next be made (1-based), for the notice.
        next_attempt: u32,
    },
    /// The restart budget is spent — the pane is left dead until the user
    /// re-opens it (which resets the budget and relaunches).
    GaveUp {
        target: SessionRef,
        last_line: String,
    },
}

/// Owns the one session the main area is currently showing.
pub struct SessionManager {
    project: String,
    connector: std::sync::Arc<dyn Connector>,
    slot: Slot,
    /// How the connect worker retries a session whose socket isn't up yet.
    retry: RetryPolicy,
    /// The last viewport size the shell asked us to report, remembered even
    /// while we are connecting. A session attaches at its spawn-time default
    /// size (e.g. the daemon's 120x40), so a resize request that went out while
    /// this slot was still connecting reached no live connection and was
    /// dropped — the shell reports the size once, when it changes, so it is
    /// never re-sent. Re-applying this size the moment a session goes live
    /// keeps the pane filling its area instead of letterboxing the default grid
    /// (`rt-review-content-session-edit-in-vi-doesn-t-fill-the-content-area`).
    last_size: Option<Size>,
    /// How to bring the current pane back before re-attaching (content re-ensure
    /// / persistent reopen marker). Shared, so the connect worker can run it.
    relauncher: Arc<dyn Relauncher>,
    /// The restart decision core for the pane currently bound (shared with the
    /// desktop app), reusing the daemon's crash-loop rules so the client
    /// supervises content sessions (and reflects a persistent pane's restarts)
    /// under the exact same cap / backoff / window. It also carries the
    /// deliberate-teardown flag (`q` / Close / quit), so that exit is not
    /// restarted. Reset when a different target is shown; re-armed when the
    /// user re-opens a given-up pane.
    restart: RestartController,
}

/// What the main area should draw right now.
pub enum MainState<'a> {
    Empty,
    Connecting(&'a SessionRef),
    Live(&'a TerminalPane),
    /// An idle workspace: render its placeholder (`rt-tui-idle-workspace-open`).
    Idle(&'a IdleInfo),
    Failed(&'a SessionRef, &'a str),
    /// The pane exited and is being auto-restarted: `(attempt, max)`. The view
    /// shows "Session exited — restarting (attempt/max)…".
    Restarting(u32, u32),
    /// The restart budget is spent; the field is the last output line. The view
    /// shows "Stopped after N restarts — select it again to relaunch" plus that
    /// line.
    GaveUp(&'a str),
}

impl SessionManager {
    pub fn new(project: impl Into<String>, connector: std::sync::Arc<dyn Connector>) -> Self {
        Self {
            project: project.into(),
            connector,
            slot: Slot::Empty,
            retry: RetryPolicy::default(),
            last_size: None,
            relauncher: Arc::new(NoopRelauncher),
            restart: RestartController::default(),
        }
    }

    /// Install the relauncher that brings this manager's panes back
    /// (content re-ensure / persistent reopen marker). Chained after
    /// [`new`](Self::new) so the two-argument constructor and the many existing
    /// call sites (and seam tests) stay unchanged.
    pub fn with_relauncher(mut self, relauncher: Arc<dyn Relauncher>) -> Self {
        self.relauncher = relauncher;
        self
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
        // Re-opening the *same* target that had failed / given up / is mid-restart
        // is the "navigate away and back relaunches it" gesture: reset the
        // crash-loop budget and run a reopen relaunch (content re-ensure, or a
        // persistent pane's daemon-budget reset marker). Any other `show` is a
        // fresh binding with a clean budget and no relaunch step — the caller
        // already ensured a content session before switching to it.
        let reopen = self.current_target() == Some(&target)
            && matches!(
                self.slot,
                Slot::Failed { .. } | Slot::GaveUp { .. } | Slot::AwaitingRestart { .. }
            );
        self.restart.begin(reopen);
        self.start_connect(target, if reopen { Some(true) } else { None }, None);
    }

    /// Spawn a connect worker for `target`. `relaunch` is `Some(reopen)` to run a
    /// relaunch step first (`reopen` distinguishes an auto-restart from a
    /// user-driven reopen); `None` for a plain first connect. `restart_attempt`
    /// tags a supervisor-kicked restart so the view shows its progress.
    fn start_connect(
        &mut self,
        target: SessionRef,
        relaunch: Option<bool>,
        restart_attempt: Option<u32>,
    ) {
        let (tx, rx) = mpsc::channel();
        // The connector and relauncher are shared, so hand the worker Arc clones
        // it borrows; the manager keeps its own.
        let job = ConnectJob {
            connector: self.connector.clone(),
            project: self.project.clone(),
            target: target.clone(),
            tx,
            retry: self.retry,
            relaunch: relaunch.map(|reopen| RelaunchStep {
                relauncher: self.relauncher.clone(),
                reopen,
            }),
        };
        let join = std::thread::Builder::new()
            .name("shelbi-shell-connect".into())
            .spawn(move || job.run())
            .expect("spawn connect worker");
        self.slot = Slot::Connecting {
            target,
            rx,
            _join: join,
            restart_attempt,
        };
    }

    /// Mark the pane as being torn down deliberately (the shell's `q` / Close /
    /// quit), so the exit that follows is read as intentional and the supervisor
    /// stands down instead of restarting it. Cleared on the next [`show`].
    pub fn note_deliberate_close(&mut self) {
        self.restart.note_deliberate_close();
    }

    /// Poll the in-flight connect without blocking and advance the restart
    /// supervisor. Returns `true` when the slot changed (a connect finished or
    /// failed, a pane exited, or a restart fired) so the caller redraws.
    pub fn poll(&mut self) -> bool {
        self.poll_at(Instant::now())
    }

    /// [`poll`](Self::poll) with the clock injected, so a test can advance past
    /// the restart backoff without sleeping.
    fn poll_at(&mut self, now: Instant) -> bool {
        let mut changed = self.poll_connect();
        changed |= self.drive_supervision(now);
        changed
    }

    /// Resolve a finished connect worker (a plain connect or a restart attempt).
    /// A no-op unless the slot is `Connecting`.
    fn poll_connect(&mut self) -> bool {
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
            Ok(c) => {
                let live = LiveSlot {
                    target,
                    conn: c.conn,
                    events: c.events,
                    pane: TerminalPane::new(Size::new(c.size.cols, c.size.rows)),
                };
                // The session attached at its spawn-time default size. If the
                // shell asked for a viewport size while we were still
                // connecting, that request reached no live connection; re-apply
                // it now so the just-attached session reflows to fill its area
                // (the PTY resize echoes back a `Resized` event that updates the
                // pane). Without this the content view letterboxes the default
                // grid — e.g. "Edit in Vi" opening vim shorter than the content
                // area (`rt-review-content-session-edit-in-vi-doesn-t-fill-the-content-area`).
                if let Some(size) = self.last_size {
                    let _ = live.conn.resize(size.cols.max(1), size.rows.max(1));
                }
                Slot::Live(Box::new(live))
            }
            Err(ConnectFailure::Idle(info)) => Slot::Idle { target, info },
            // The worker only ever forwards a terminal outcome: it retries the
            // transient `Starting`/`Awaiting` signals itself and, on give-up,
            // converts them to a `Message`. One arriving here would mean the
            // worker didn't retry, so treat it as a plain failure rather than
            // silently dropping it.
            // The TUI's connector is local-only, so `Unreachable` (a remote
            // machine's relay) never arrives; were it to, it is a failure.
            Err(
                ConnectFailure::Starting(error)
                | ConnectFailure::Awaiting(error)
                | ConnectFailure::Message(error)
                | ConnectFailure::Unreachable(error),
            ) => Slot::Failed { target, error },
        };
        true
    }

    /// Emit a `supervision=` line for a *content* session's restart / give-up,
    /// identifying the pane as `<slot-or-workspace>/<role>`. Persistent panes
    /// (orchestrator / agents / review slot) are the daemon supervisor's to log,
    /// so they are skipped here to avoid double-logging; this covers the content
    /// sessions the client supervises itself. Best-effort — a failed append is a
    /// missed log line, not a broken restart.
    fn emit_content_supervision(&self, target: &SessionRef, action: &str, reason: &str) {
        let label = match target {
            SessionRef::Review { slot, role } => format!("{slot}/{role}"),
            SessionRef::WorkspaceContent { workspace, role } => format!("{workspace}/{role}"),
            SessionRef::Orchestrator | SessionRef::Workspace(_) => return,
        };
        let _ = shelbi_state::append_supervision_event(&self.project, Some(&label), action, reason);
    }

    /// Advance the crash-loop supervisor for the current pane. Feeds the pure
    /// decision core one observation per call (reusing the daemon's cap /
    /// backoff / window), turning an unexpected exit — or a reconnect that found
    /// no live session — into a restart, a backoff wait, or a give-up. Returns
    /// whether the slot changed.
    fn drive_supervision(&mut self, now: Instant) -> bool {
        // Gather the observation without holding a borrow across the mutation.
        enum Observed {
            /// The bound pane is up — feed an alive tick (recovery reset).
            Alive,
            /// The pane is dead / failed and may need restarting.
            Dead { target: SessionRef, last_line: String },
            /// Nothing to supervise this tick (connecting, idle, empty, given up).
            Nothing,
        }
        let observed = match &self.slot {
            Slot::Live(live) if live.pane.exited.is_none() => Observed::Alive,
            Slot::Live(live) => Observed::Dead {
                target: live.target.clone(),
                last_line: exit_line(live.pane.exited.as_ref()),
            },
            Slot::Failed { target, error } => Observed::Dead {
                target: target.clone(),
                last_line: error.clone(),
            },
            Slot::AwaitingRestart {
                target, last_line, ..
            } => Observed::Dead {
                target: target.clone(),
                last_line: last_line.clone(),
            },
            Slot::Connecting { .. } | Slot::Idle { .. } | Slot::GaveUp { .. } | Slot::Empty => {
                Observed::Nothing
            }
        };

        match observed {
            Observed::Nothing => false,
            Observed::Alive => {
                // A pane that stays up past the recovery window forgets its
                // crash history; a brief flap-back keeps it so the cap still
                // trips. No slot change.
                self.restart.alive(now);
                false
            }
            Observed::Dead { target, last_line } => self.act_on_death(target, last_line, now),
        }
    }

    /// Decide what to do about a dead/failed pane and apply it. The decision is
    /// the shared [`RestartController`]'s: the pane the user is viewing is
    /// always "work" (we want it up); a deliberate teardown is the one case we
    /// don't restart.
    fn act_on_death(&mut self, target: SessionRef, last_line: String, now: Instant) -> bool {
        match self.restart.dead(now) {
            DeathAction::Restart { attempt } => {
                // Content re-ensures (the daemon won't on its own); a persistent
                // pane relaunches via the daemon, so this is an auto (non-reopen)
                // relaunch step.
                self.emit_content_supervision(&target, "restart", "crash");
                self.start_connect(target, Some(false), Some(attempt));
                true
            }
            DeathAction::GiveUp => {
                self.emit_content_supervision(&target, "gave-up", "crash-loop");
                self.slot = Slot::GaveUp { target, last_line };
                true
            }
            // Deliberate teardown: clear the pane so we stop supervising it.
            DeathAction::StandDown => {
                if matches!(self.slot, Slot::Empty) {
                    return false;
                }
                self.slot = Slot::Empty;
                true
            }
            // A pane that never came up at all (a first-open connect failure):
            // leave it showing as `Failed`.
            DeathAction::Leave => false,
            DeathAction::Backoff { next_attempt } => {
                // Inside the backoff between attempts: reflect it as
                // "restarting" so the user sees the pane is coming back. Only
                // rewrite the slot when the shown attempt changes, to avoid
                // redraw churn.
                let already = matches!(
                    &self.slot,
                    Slot::AwaitingRestart { next_attempt: n, .. } if *n == next_attempt
                );
                if already {
                    return false;
                }
                self.slot = Slot::AwaitingRestart {
                    target,
                    last_line,
                    next_attempt,
                };
                true
            }
        }
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
            | Slot::Failed { target, .. }
            | Slot::AwaitingRestart { target, .. }
            | Slot::GaveUp { target, .. } => Some(target),
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
        let max = RestartController::max_restarts();
        match &self.slot {
            Slot::Empty => MainState::Empty,
            // A supervisor-kicked restart still connecting shows its progress;
            // an ordinary first connect shows plain "Connecting…".
            Slot::Connecting {
                restart_attempt: Some(a),
                ..
            } => MainState::Restarting(*a, max),
            Slot::Connecting { target, .. } => MainState::Connecting(target),
            Slot::Live(live) => MainState::Live(&live.pane),
            Slot::Idle { info, .. } => MainState::Idle(info),
            Slot::Failed { target, error } => MainState::Failed(target, error),
            Slot::AwaitingRestart { next_attempt, .. } => {
                MainState::Restarting(*next_attempt, max)
            }
            Slot::GaveUp { last_line, .. } => MainState::GaveUp(last_line),
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

    /// Override the retry policy (test seam, so a test needn't wait the full
    /// 15 s deadline to observe the give-up).
    #[cfg(test)]
    pub(crate) fn set_retry(&mut self, retry: RetryPolicy) {
        self.retry = retry;
    }

    /// Report the main area's size to the live session so it reflows to fill
    /// it when the shell is the most-recently-active client. The size is
    /// remembered (even with no live session) and re-applied when a session
    /// next goes live, so a resize issued while a slot was still connecting is
    /// not lost
    /// (`rt-review-content-session-edit-in-vi-doesn-t-fill-the-content-area`).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let size = Size::new(cols.max(1), rows.max(1));
        self.last_size = Some(size);
        if let Slot::Live(live) = &self.slot {
            let _ = live.conn.resize(size.cols, size.rows);
        }
    }
}

/// A relaunch to run on the worker thread before connecting.
struct RelaunchStep {
    relauncher: Arc<dyn Relauncher>,
    reopen: bool,
}

/// A connect job run on a worker thread.
struct ConnectJob {
    connector: std::sync::Arc<dyn Connector>,
    project: String,
    target: SessionRef,
    tx: mpsc::Sender<Result<Connected, ConnectFailure>>,
    retry: RetryPolicy,
    /// A relaunch to perform before the connect (content re-ensure / persistent
    /// reopen marker). `None` for a plain first connect.
    relaunch: Option<RelaunchStep>,
}

impl ConnectJob {
    fn run(self) {
        // Bring the pane back first, when asked. A relaunch failure is terminal
        // and surfaced as the connect error, so the view shows a clear message
        // (with the carried reason) rather than retrying a relaunch that can't
        // happen.
        if let Some(step) = &self.relaunch {
            if let Err(e) = step.relauncher.relaunch(&self.target, step.reopen) {
                let _ = self.tx.send(Err(ConnectFailure::Message(format!(
                    "couldn't relaunch {}: {e}",
                    self.target.display()
                ))));
                return;
            }
        }
        // Retry a session whose socket isn't up yet (or an orchestrator that is
        // being relaunched) on the shared policy; every other outcome is
        // terminal and forwarded at once. Running the wait here, on the worker
        // thread, keeps the UI loop non-blocking — it only ever polls.
        let result = shelbi_app::session::retry_connect(
            self.connector.as_ref(),
            &self.project,
            &self.target,
            self.retry,
        );
        // The UI may have moved on; a failed send just means nobody is waiting.
        let _ = self.tx.send(result);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::session::GridSize;
    use shelbi_app::supervision::{
        SupervisionAction, SupervisionInputs, SupervisionState, MAX_RESTARTS_IN_WINDOW,
    };
    use shelbi_proto::capability;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;
    use std::time::Duration;

    fn connecting(m: &SessionManager) -> bool {
        matches!(m.state(), MainState::Connecting(_))
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
                Ok(info) => GridSize::new(info.cols.max(1), info.rows.max(1)),
                Err(_) => GridSize::new(80, 24),
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
                Ok(info) => GridSize::new(info.cols.max(1), info.rows.max(1)),
                Err(_) => GridSize::new(80, 24),
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
        // Generous deadline (30s, not 5s): the session spawns on a worker thread,
        // and under a loaded host its socket can take seconds to bind. The loop
        // breaks the instant the socket appears, so a healthy run is unaffected.
        for _ in 0..3000 {
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
        // Generous deadline (30s): attaching involves a real session spawn plus a
        // connect handshake, both of which stretch under load. Returns the moment
        // the pane goes live, so a healthy run doesn't pay the full budget.
        for _ in 0..3000 {
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

    /// A connector that opens a real session at its spawn size, but blocks
    /// inside `connect` until the test releases its gate — so the test can issue
    /// a resize while the slot is still connecting (the window in which the
    /// shell's one-shot resize report would otherwise reach no live connection).
    struct GatedLive {
        sock: std::path::PathBuf,
        gate: std::sync::Mutex<Option<Receiver<()>>>,
    }

    impl Connector for GatedLive {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            if let Some(rx) = self.gate.lock().unwrap().take() {
                let _ = rx.recv(); // released when the test drops its sender
            }
            let (conn, events) = Connection::open(&self.sock, None, capability::ALL)
                .map_err(|e| ConnectFailure::Message(e.to_string()))?;
            let size = match conn.info() {
                Ok(info) => GridSize::new(info.cols.max(1), info.rows.max(1)),
                Err(_) => GridSize::new(80, 24),
            };
            conn.attach(None)
                .map_err(|e| ConnectFailure::Message(e.to_string()))?;
            Ok(Connected { conn, events, size })
        }
    }

    /// Drive the manager until its live pane reports `want` as its session size
    /// (pumping the in-band `Resized` the real session emits after a resize), or
    /// the budget runs out.
    fn wait_until_pane_size(mgr: &mut SessionManager, want: Size) -> bool {
        let mut ring = false;
        for _ in 0..500 {
            mgr.poll();
            mgr.pump_output(&mut ring);
            if let MainState::Live(pane) = mgr.state() {
                if pane.session_size() == want {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_resize_while_connecting_is_applied_when_the_session_goes_live() {
        // A content session attaches at its spawn-time default size (here the
        // cat session's 80x24, standing in for the daemon's default). The shell
        // reports the content rect's size once, while the slot is still
        // connecting, so that resize reaches no live connection. The manager
        // must remember it and re-apply it the moment the session goes live, so
        // the pane reflows to fill its area instead of letterboxing the default
        // grid — the "Edit in Vi opens vim shorter than the content area" bug
        // (`rt-review-content-session-edit-in-vi-doesn-t-fill-the-content-area`).
        let _lock = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let (sock, _cleanup) = spawn_cat_session(home.path(), "review/editor");

        let (gate_tx, gate_rx) = mpsc::channel();
        let connector = std::sync::Arc::new(GatedLive {
            sock,
            gate: std::sync::Mutex::new(Some(gate_rx)),
        });
        let mut mgr = SessionManager::new("review", connector);
        mgr.show(SessionRef::Review {
            slot: "r1".into(),
            role: "editor".into(),
        });
        assert!(connecting(&mgr), "the gated connect has not completed yet");

        // Resize while still connecting: there is no live connection to receive
        // it, so without the fix it is simply lost.
        mgr.resize(50, 10);
        assert!(connecting(&mgr), "a resize does not complete the connect");

        // Release the gate: the session attaches at its 80x24 spawn size.
        drop(gate_tx);

        // Once live, the remembered 50x10 is re-applied, so the real session
        // reflows and the pane ends up at the requested size, not the default.
        assert!(
            wait_until_pane_size(&mut mgr, Size::new(50, 10)),
            "the session should reflow to the size requested while connecting"
        );
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

    /// A connector that opens a *real* session socket which accepts the
    /// connection but never answers the hello — the live review hang. It bounds
    /// the connect with a short handshake timeout and maps the result the way
    /// [`LiveConnector`] does, so the manager must settle in `Failed`, not sit on
    /// `Connecting` (`rt-review-screen-hangs-on-connecting`).
    struct SilentSocketConnector {
        sock: std::path::PathBuf,
    }

    impl Connector for SilentSocketConnector {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            match Connection::open_with_timeout(
                &self.sock,
                None,
                capability::ALL,
                Duration::from_millis(150),
            ) {
                Ok(_) => Err(ConnectFailure::Message("unexpected hello".into())),
                Err(e) => Err(ConnectFailure::Message(e.to_string())),
            }
        }
    }

    #[test]
    fn live_connector_attaches_to_the_live_session_not_a_same_named_zombie() {
        // Two sessions share the discovery name `ztest/ws/review`: a real live
        // `cat` session, and an "alive but not listening" zombie (lock held, but
        // its socket refuses every connect) that is even *newer*. The production
        // `LiveConnector` must choose the live one — a successful attach proves it,
        // since landing on the zombie would have failed the connect
        // (`rt-re-entering-a-review-fails-to-attach`).
        use shelbi_session::lock::SessionLock;
        use std::os::unix::net::UnixListener;

        let _lock = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let (_live_sock, _cleanup) = spawn_cat_session(home.path(), "ztest/ws/review");

        // A same-named zombie dir: a held lock (so discovery reports it alive), a
        // far-future launch time (so a naive "prefer newest" would pick it), and a
        // bound-then-dropped socket (so every connect is refused).
        let zid = shelbi_session::layout::derive_id("ztest/ws/review", 999_999, 1);
        let zdir = home.path().join("sessions").join(&zid);
        std::fs::create_dir_all(&zdir).unwrap();
        let zmeta = shelbi_session::Meta {
            id: zid.clone(),
            name: "ztest/ws/review".into(),
            argv: vec!["cat".into()],
            cwd: zdir.clone(),
            task: None,
            launched_at: "2999-01-01T00:00:00Z".into(),
            protocol_version: shelbi_proto::PROTOCOL_VERSION,
            pid: 0,
        };
        std::fs::write(zdir.join("meta.json"), zmeta.to_json().unwrap()).unwrap();
        let _zlock = SessionLock::acquire(&zdir.join("lock")).expect("hold the zombie's lock");
        let zsock = zdir.join("sock");
        let z = UnixListener::bind(&zsock).unwrap();
        drop(z);
        if !zsock.exists() {
            return; // platform removed the socket file on drop; refusing case N/A
        }

        match LiveConnector.connect("ztest", &SessionRef::Workspace("review".into())) {
            // We reached a live binding (info + attach succeeded against the cat).
            Ok(connected) => assert!(connected.size.cols >= 1),
            Err(_) => panic!("the connector must attach to the live session, not the zombie"),
        }
    }

    // -- restart supervision (rt-auto-restart-killed-panes) ------------------

    /// A relauncher that records every call, so a test can prove the manager
    /// asked for a relaunch (and whether it was a reopen) without a daemon.
    #[derive(Default)]
    struct CountingRelauncher {
        calls: std::sync::Mutex<Vec<bool>>, // each entry = the `reopen` flag
    }
    impl Relauncher for CountingRelauncher {
        fn relaunch(&self, _target: &SessionRef, reopen: bool) -> Result<(), String> {
            self.calls.lock().unwrap().push(reopen);
            Ok(())
        }
    }

    /// Build a `SupervisionState` that has already given up, using only its
    /// public API (the fields are private to `shelbi_app::supervision`): a live
    /// sighting then MAX_RESTARTS_IN_WINDOW crashes, each past its backoff, then
    /// one more crash to trip give-up.
    fn gave_up_state() -> SupervisionState {
        let mut s = SupervisionState::default();
        let mut t = Instant::now();
        let alive = SupervisionInputs {
            alive: true,
            intentional_shutdown: false,
            has_work: false,
        };
        let dead = SupervisionInputs {
            alive: false,
            intentional_shutdown: false,
            has_work: true,
        };
        for n in 0..MAX_RESTARTS_IN_WINDOW {
            s.decide(&alive, t);
            t += shelbi_app::supervision::BASE_BACKOFF * (1u32 << n)
                + Duration::from_secs(1);
            assert_eq!(s.decide(&dead, t), SupervisionAction::Restart);
        }
        assert_eq!(s.decide(&dead, t), SupervisionAction::GiveUp);
        s
    }

    #[test]
    fn reopening_a_given_up_pane_resets_the_budget_and_relaunches() {
        // The user navigates away and back to a pane whose restarts were spent:
        // the budget resets and a reopen relaunch runs (reopen=true), rather than
        // the view sitting on the dead state.
        let relauncher = std::sync::Arc::new(CountingRelauncher::default());
        let (_tx, rx): (Sender<()>, Receiver<()>) = mpsc::channel();
        let connector = BlockingConnector {
            release: std::sync::Mutex::new(Some(rx)),
        };
        let mut mgr = SessionManager::new("demo", std::sync::Arc::new(connector))
            .with_relauncher(relauncher.clone());
        let target = SessionRef::Orchestrator;
        // Put it in the given-up state the daemon/client would reach.
        mgr.restart = RestartController::with_state(gave_up_state());
        assert_eq!(mgr.restart.restart_count(), MAX_RESTARTS_IN_WINDOW);
        mgr.slot = Slot::GaveUp {
            target: target.clone(),
            last_line: "boom".into(),
        };
        assert!(matches!(mgr.state(), MainState::GaveUp("boom")));

        // Re-open the same target.
        mgr.show(target.clone());
        // Budget reset and a connect kicked.
        assert_eq!(mgr.restart.restart_count(), 0, "the budget is reset");
        assert!(matches!(mgr.slot, Slot::Connecting { .. }));
        // The worker ran a reopen relaunch; wait briefly for it.
        let mut saw_reopen = false;
        for _ in 0..200 {
            if relauncher.calls.lock().unwrap().iter().any(|&reopen| reopen) {
                saw_reopen = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_reopen, "a reopen relaunch (reopen=true) should run");
    }

    /// Spawn a real session that stays up briefly then exits, so a connected
    /// client observes a genuine `Exited`. Returns a cleanup guard.
    fn spawn_exiting_session(home: &std::path::Path, name: &str) -> CatCleanup {
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
                "stty raw -echo 2>/dev/null; sleep 0.4; exit 7".into(),
            ],
            manage_daemon: false,
        };
        std::thread::spawn(move || shelbi_session::run(args));
        let sock = paths.sock();
        // Generous deadline (30s, not 5s): the session spawns on a worker thread,
        // and under a loaded host its socket can take seconds to bind. The loop
        // breaks the instant the socket appears, so a healthy run is unaffected.
        for _ in 0..3000 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(sock.exists(), "the session socket should appear");
        CatCleanup { sock }
    }

    /// Drive the manager until its bound pane reports it exited, pumping output.
    fn wait_until_exited(mgr: &mut SessionManager) -> bool {
        let mut ring = false;
        // Generous deadline (30s): the child exits on its own after a short sleep,
        // but observing that exit depends on scheduling the poll loop, which lags
        // under load. Returns as soon as the exit is seen.
        for _ in 0..3000 {
            mgr.poll();
            mgr.pump_output(&mut ring);
            if let Slot::Live(live) = &mgr.slot {
                if live.pane.exited.is_some() {
                    return true;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_live_pane_that_exits_is_auto_restarted() {
        // A pane that was alive and then its child exits is brought back: the
        // manager leaves Live, asks the relauncher for a (non-reopen) relaunch,
        // and shows the restart in flight.
        let _lock = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let _cleanup = spawn_exiting_session(home.path(), "demo/ws/worker");

        let relauncher = std::sync::Arc::new(CountingRelauncher::default());
        let mut mgr = SessionManager::new("demo", std::sync::Arc::new(LiveConnector))
            .with_relauncher(relauncher.clone());
        mgr.retry = RetryPolicy {
            deadline: Duration::from_millis(200),
            backoff: Duration::from_millis(10),
        };
        mgr.show(SessionRef::Workspace("worker".into()));
        assert!(poll_until_live(&mut mgr), "the session attaches live first");
        assert!(wait_until_exited(&mut mgr), "its child then exits");

        // The next poll acts on the exit: first restart fires immediately.
        let base = Instant::now();
        mgr.poll_at(base);
        assert!(
            matches!(mgr.state(), MainState::Restarting(1, _)),
            "the pane shows restart attempt 1 in flight"
        );
        // A (non-reopen) relaunch was requested. It runs on the connect worker
        // thread `poll_at` spawned, so it lands a beat after `poll_at` returns —
        // wait for it rather than reading the shared log straight away (the read
        // used to race the worker and see `[]`). Only one restart worker was
        // kicked (a single `poll_at`), so once a call shows up the log is final.
        let calls = {
            let mut calls = Vec::new();
            for _ in 0..500 {
                calls = relauncher.calls.lock().unwrap().clone();
                if !calls.is_empty() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            calls
        };
        assert_eq!(calls, vec![false], "one auto (non-reopen) relaunch");
    }

    #[test]
    fn a_deliberately_closed_pane_is_not_restarted() {
        // When the shell marks a deliberate teardown, the exit that follows is
        // not restarted — it stands down instead.
        let _lock = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let _cleanup = spawn_exiting_session(home.path(), "demo/ws/stopme");

        let relauncher = std::sync::Arc::new(CountingRelauncher::default());
        let mut mgr = SessionManager::new("demo", std::sync::Arc::new(LiveConnector))
            .with_relauncher(relauncher.clone());
        mgr.show(SessionRef::Workspace("stopme".into()));
        assert!(poll_until_live(&mut mgr));
        assert!(wait_until_exited(&mut mgr));

        // The teardown was deliberate.
        mgr.note_deliberate_close();
        mgr.poll_at(Instant::now());
        assert!(
            !matches!(mgr.state(), MainState::Restarting(..)),
            "a deliberate close is not restarted"
        );
        assert!(
            relauncher.calls.lock().unwrap().is_empty(),
            "no relaunch is requested for a deliberate close"
        );
    }

    #[test]
    fn a_session_that_accepts_but_never_answers_ends_in_failed_not_connecting() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let accepted = std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                std::thread::sleep(Duration::from_secs(2));
                drop(stream);
            }
        });

        let mut mgr = SessionManager::new(
            "demo",
            std::sync::Arc::new(SilentSocketConnector { sock }),
        );
        let start = Instant::now();
        mgr.show(SessionRef::Workspace("review".into()));
        poll_until_settled(&mut mgr);
        let elapsed = start.elapsed();

        // It did not sit on "Connecting…" — it resolved to a terminal failure
        // carrying the handshake-timeout message.
        match mgr.state() {
            MainState::Failed(_, err) => assert!(
                err.contains("handshake timeout"),
                "the failure should name the handshake timeout, got {err:?}"
            ),
            _ => panic!("a silent session must land in MainState::Failed, not Connecting"),
        }
        assert!(
            elapsed < Duration::from_secs(1),
            "the connect ended near the bound, not forever (took {elapsed:?})"
        );
        let _ = accepted.join();
    }
}
