//! A session in the main area: one terminal pane bound to one of the
//! project's sessions, kept attached across the session exiting, being
//! relaunched, or (on another machine) its connection dropping.
//!
//! [`SessionView`] is the desktop twin of the TUI's `SessionManager` and keeps
//! its behavior, built from the same toolkit-free pieces in
//! `shelbi_app::session`. The Chat view (`chat.rs`, the orchestrator) is one;
//! the Workspace view (`workspace.rs`) holds two, the agent (or its editor or
//! diff) and the user shell beside it.
//!
//! - **Attach on show, detach on hide.** Showing the view attaches with a full
//!   replay (so the scrollback is there at once); hiding it (another view, the
//!   settings) drops the pane, which closes the connection.
//! - **Relaunch-tolerant connect.** The connect runs off the UI thread through
//!   [`shelbi_app::session::retry_connect`], so a socket not bound yet, or an
//!   orchestrator caught between its old session exiting and the relaunched
//!   one binding (`shelbi reload`, a crash, a daemon restart), is retried
//!   rather than settled as a permanent attach error
//!   (`rt-tui-shows-a-permanent-attach-error-when-the-orchestrator-is-relaunched`).
//! - **Restart states.** When the attached session exits (or its connection
//!   drops), the shared [`RestartController`] decides: reattach now
//!   ("Session exited — restarting (N/5)…"), wait out the backoff, give up
//!   ("Stopped after 5 restarts — select it again to relaunch"), or stand down
//!   after a deliberate teardown (quitting the project). Daemon-supervised
//!   panes (the orchestrator, an agent) only re-attach on an automatic
//!   restart; content sessions are re-ensured through the daemon; re-opening a
//!   given-up pane drops the daemon's relaunch marker (the view's
//!   [`Relauncher`]). A pane with [`OnDeath::Close`] (the user shell) is not
//!   restarted: its exit closes it.
//! - **Reconnect** (`dt-workspace-view`). A session on another machine is
//!   reached through the machine's relay over SSH. When that connection drops,
//!   the stream ends with no exit while the session keeps running on the
//!   remote, so the view reconnects ("Reconnecting to <machine>… (attempt N)")
//!   on the shared [`ReconnectController`]'s backoff, and only an attempt that
//!   reaches the machine and finds the session gone counts as a death.
//! - **Idle.** A declared workspace with no session shows the TUI's idle
//!   placeholder, and [`SessionView::retry_if_idle`] re-attaches once a session
//!   appears (`rt-tui-idle-workspace-open`).
//! - **Sizing** is the terminal element's: it reports its size (debounced) and
//!   the session follows the most recently active client, so a TUI attached at
//!   another size reflows whenever either client types.
//!
//! The connect seam ([`SessionConnector`]), the relauncher and the clock are
//! injected, so the attach state machine is tested in a gpui test context with
//! a fake session (below).

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, AnyWindowHandle, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Subscription, Task, Window,
};
use shelbi_app::session::{
    self as app_session, ConnectFailure, DeathAction, IdleInfo, PersistentRelauncher,
    ReconnectController, ReconnectPolicy, Relauncher, RestartController, RetryPolicy, SessionRef,
};
use shelbi_term::Size;

use crate::elements::terminal::session::{self as pane_session, Connected};
use crate::elements::terminal::{PaneEvent, TerminalPane};
use crate::refresh::SessionSink;
use crate::theme::Theme;

/// How often a pane waiting out its restart backoff re-asks the controller
/// (the TUI asks once per event-loop turn). Only runs while waiting.
const RESTART_TICK: Duration = Duration::from_millis(250);

/// The blocking work of reaching a session: the relaunch-tolerant connect and
/// wiring it onto the pane's reader/writer threads. Injectable so the view's
/// state machine runs against a fake session in tests.
pub trait SessionConnector: Send + Sync {
    /// Connect to `target` in `project`, retrying transient outcomes on
    /// `retry`. Blocking: the view runs it on the background executor.
    fn connect(
        &self,
        project: &str,
        target: &SessionRef,
        retry: RetryPolicy,
    ) -> Result<Connected, ConnectFailure>;
}

/// The production connector: [`shelbi_orchestrator::session_connect`]'s
/// attach with replay (this machine's sessions directory, or the workspace
/// machine's relay), retried, then wired onto the pane's threads.
pub struct LiveConnector;

struct AppConnector;

impl app_session::Connector for AppConnector {
    fn connect(
        &self,
        project: &str,
        target: &SessionRef,
    ) -> Result<app_session::Connected, ConnectFailure> {
        shelbi_orchestrator::session_connect::connect(project, target)
    }
}

impl SessionConnector for LiveConnector {
    fn connect(
        &self,
        project: &str,
        target: &SessionRef,
        retry: RetryPolicy,
    ) -> Result<Connected, ConnectFailure> {
        let c = app_session::retry_connect(&AppConnector, project, target, retry)?;
        pane_session::wire(
            c.conn,
            c.events,
            Size::new(c.size.cols, c.size.rows),
            app_session::discovery_name(project, target),
        )
        .map_err(ConnectFailure::Message)
    }
}

/// What the view needs from the outside world.
pub struct SessionDeps {
    pub connector: Arc<dyn SessionConnector>,
    pub relauncher: Arc<dyn Relauncher>,
    pub retry: RetryPolicy,
    /// How a remote pane's reconnect backs off.
    pub reconnect: ReconnectPolicy,
    /// The supervision clock (the restart backoff and recovery windows, the
    /// reconnect backoff).
    pub clock: Rc<dyn Fn() -> Instant>,
}

impl SessionDeps {
    /// The real sessions, `relauncher`, and the wall clock.
    pub fn live_with(relauncher: Arc<dyn Relauncher>) -> Self {
        Self {
            connector: Arc::new(LiveConnector),
            relauncher,
            retry: RetryPolicy::default(),
            reconnect: ReconnectPolicy::default(),
            clock: Rc::new(Instant::now),
        }
    }

    /// The real session, the orchestrator's daemon-side relaunch markers, and
    /// the wall clock.
    pub fn live(project: &str) -> Self {
        Self::live_with(Arc::new(PersistentRelauncher {
            project: project.to_string(),
        }))
    }
}

/// What the view does when its session dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnDeath {
    /// Bring it back on the restart controller (the orchestrator, an agent,
    /// an editor or diff).
    Restart,
    /// Close the pane and tell the owner ([`SessionViewEvent::Ended`]): the
    /// user shell, which the user ends by exiting it.
    Close,
}

/// What the view tells its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionViewEvent {
    /// The terminal took keyboard focus (a click, or the view being shown):
    /// the window's focus model moves to the main area.
    Focused,
    /// An [`OnDeath::Close`] pane's session ended; the view is empty and
    /// hidden.
    Ended,
}

/// The view's binding to its session.
enum Slot {
    /// Not showing anything (hidden, or stood down after a deliberate close).
    Empty,
    /// A connect (or a restart's relaunch + connect) is in flight.
    /// `restart_attempt` is the 1-based restart it serves, `None` for a first
    /// connect, so the view shows "Connecting…" rather than "restarting".
    Connecting { restart_attempt: Option<u32> },
    /// Attached and streaming.
    Live(Entity<TerminalPane>),
    /// A declared workspace with no session (never the orchestrator).
    Idle(IdleInfo),
    /// The connect settled on a failure.
    Failed(String),
    /// The pane exited and is waiting out its restart backoff.
    AwaitingRestart {
        last_line: String,
        next_attempt: u32,
    },
    /// The restart budget is spent; re-selecting the view relaunches it.
    GaveUp { last_line: String },
    /// A remote session's machine is unreachable: waiting for (or running) a
    /// reconnect attempt. The attempt and reason are the controller's.
    Reconnecting,
}

/// The view's state without its payloads (the tests' view of the machine).
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionPhase {
    Empty,
    Connecting,
    Live,
    Idle,
    Failed(String),
    /// `(attempt, max)`: a restart in flight or waiting out its backoff.
    Restarting(u32, u32),
    GaveUp(String),
    /// The reconnect attempt waited for or in flight.
    Reconnecting(u32),
}

/// One session in the main area of a project window.
pub struct SessionView {
    project: String,
    target: SessionRef,
    /// The `View::Session` name this view is shown for (`orch`, a workspace),
    /// which also keys its title in the window's session notices.
    view_name: String,
    window: AnyWindowHandle,
    deps: SessionDeps,
    on_death: OnDeath,
    /// The machine the session runs on when it is remote (reached through a
    /// relay): a connection that ends with no exit is then a lost link to
    /// reconnect, not a death.
    remote_machine: Option<String>,
    /// The palette chord the idle placeholder names.
    palette_chord: String,
    slot: Slot,
    restart: RestartController,
    reconnect: ReconnectController,
    /// Whether the window currently shows this view.
    shown: bool,
    /// Hidden while dead (failed, given up, mid-restart): the next show is a
    /// re-open (the "navigate away and back relaunches it" gesture).
    reopen_on_show: bool,
    /// Bumped by every connect and teardown; a connect result from an older
    /// generation is dropped because the view has moved on.
    generation: u64,
    /// Where the pane's title and exit go (the window's session notices).
    sink: Option<SessionSink>,
    /// The focus handle of a pane that held keyboard focus when its session
    /// ended, so the pane that replaces it after a restart takes focus too (the
    /// old pane goes away with its handle).
    dead_pane_focus: Option<FocusHandle>,
    focus_handle: FocusHandle,
    _connect: Option<Task<()>>,
    _tick: Option<Task<()>>,
    _pane_subscriptions: Vec<Subscription>,
}

impl EventEmitter<SessionViewEvent> for SessionView {}

impl SessionView {
    /// A view of `target` in `project`, in `window`, shown under the
    /// `View::Session` name `view_name`. Nothing connects until
    /// [`show`](Self::show).
    pub fn new(
        project: &str,
        target: SessionRef,
        view_name: &str,
        window: AnyWindowHandle,
        deps: SessionDeps,
        sink: Option<SessionSink>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            project: project.to_string(),
            target,
            view_name: view_name.to_string(),
            window,
            reconnect: ReconnectController::new(deps.reconnect),
            deps,
            on_death: OnDeath::Restart,
            remote_machine: None,
            palette_chord: String::new(),
            slot: Slot::Empty,
            restart: RestartController::default(),
            shown: false,
            reopen_on_show: false,
            generation: 0,
            sink,
            dead_pane_focus: None,
            focus_handle: cx.focus_handle(),
            _connect: None,
            _tick: None,
            _pane_subscriptions: Vec::new(),
        }
    }

    /// The orchestrator chat for `project`, in `window`.
    pub fn orchestrator(
        project: &str,
        window: AnyWindowHandle,
        deps: SessionDeps,
        sink: Option<SessionSink>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new(project, SessionRef::Orchestrator, "orch", window, deps, sink, cx)
    }

    /// What the view does when its session dies (default [`OnDeath::Restart`]).
    pub fn set_on_death(&mut self, on_death: OnDeath) {
        self.on_death = on_death;
    }

    /// The machine the session runs on when it is remote, `None` when local.
    pub fn set_remote_machine(&mut self, machine: Option<String>) {
        self.remote_machine = machine;
    }

    /// The palette chord the idle placeholder names.
    pub fn set_palette_chord(&mut self, chord: &str) {
        self.palette_chord = chord.to_string();
    }

    /// The session this view shows.
    #[cfg(test)]
    pub fn target(&self) -> &SessionRef {
        &self.target
    }

    /// Whether the window shows this view.
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The view's state.
    #[cfg(test)]
    pub fn phase(&self) -> SessionPhase {
        let max = RestartController::max_restarts();
        match &self.slot {
            Slot::Empty => SessionPhase::Empty,
            Slot::Connecting {
                restart_attempt: Some(a),
            } => SessionPhase::Restarting(*a, max),
            Slot::Connecting { .. } => SessionPhase::Connecting,
            Slot::Live(_) => SessionPhase::Live,
            Slot::Idle(_) => SessionPhase::Idle,
            Slot::Failed(e) => SessionPhase::Failed(e.clone()),
            Slot::AwaitingRestart { next_attempt, .. } => {
                SessionPhase::Restarting(*next_attempt, max)
            }
            Slot::GaveUp { last_line } => SessionPhase::GaveUp(last_line.clone()),
            Slot::Reconnecting => SessionPhase::Reconnecting(self.reconnect.attempt()),
        }
    }

    /// The live pane, if any.
    pub fn pane(&self) -> Option<&Entity<TerminalPane>> {
        match &self.slot {
            Slot::Live(pane) => Some(pane),
            _ => None,
        }
    }

    /// The text the main area shows instead of the terminal, worded as the
    /// TUI words it. An idle workspace shows [`Self::idle_lines`] instead.
    pub fn notice(&self) -> Option<String> {
        let max = RestartController::max_restarts();
        Some(match &self.slot {
            Slot::Live(_) => return None,
            Slot::Empty => "No session".to_string(),
            Slot::Connecting {
                restart_attempt: None,
            } => app_session::connecting_notice(&self.target),
            Slot::Connecting {
                restart_attempt: Some(a),
            } => app_session::restarting_notice(*a, max),
            Slot::AwaitingRestart { next_attempt, .. } => {
                app_session::restarting_notice(*next_attempt, max)
            }
            Slot::Idle(info) => format!("No live session for {}", info.name),
            Slot::Failed(e) => app_session::failed_notice(&self.target, e),
            Slot::GaveUp { last_line } => app_session::gave_up_notice(last_line),
            Slot::Reconnecting => app_session::reconnecting_notice(
                self.remote_machine.as_deref().unwrap_or("the machine"),
                self.reconnect.attempt().max(1),
                self.reconnect.reason(),
            ),
        })
    }

    /// The idle-workspace placeholder's lines (the TUI's), while idle.
    pub fn idle_lines(&self) -> Option<Vec<String>> {
        match &self.slot {
            Slot::Idle(info) => Some(app_session::idle_placeholder_lines(info, &self.palette_chord)),
            _ => None,
        }
    }

    /// Show the view: attach (with replay) unless already connecting or live.
    /// Showing it again while it is failed, given up or mid-restart re-opens it:
    /// the restart budget resets and the daemon's budget is reset too (a
    /// reopen relaunch), as re-selecting a pane does in the TUI.
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if self.shown
            && matches!(
                self.slot,
                Slot::Connecting { .. } | Slot::Live(_) | Slot::Reconnecting
            )
        {
            return;
        }
        let dead = matches!(
            self.slot,
            Slot::Failed(_) | Slot::GaveUp { .. } | Slot::AwaitingRestart { .. }
        );
        let reopen = (self.shown && dead) || (!self.shown && self.reopen_on_show);
        self.shown = true;
        self.reopen_on_show = false;
        self.reconnect.connected();
        self.restart.begin(reopen);
        self.start_connect(reopen.then_some(true), None, cx);
    }

    /// Show `target` instead of whatever the view shows: a different target
    /// starts fresh (the old pane is dropped, the budgets reset); the same one
    /// is a plain [`show`](Self::show) (a re-select re-opens a dead pane).
    pub fn show_target(&mut self, target: SessionRef, cx: &mut Context<Self>) {
        if target != self.target {
            self.target = target;
            self.shown = false;
            self.reopen_on_show = false;
            self.restart = RestartController::default();
            self.clear(cx);
        }
        self.show(cx);
    }

    /// Hide the view: drop the pane (closing the connection) and any connect
    /// or restart in flight. A dead pane remembers to re-open on the next show.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        self.reopen_on_show = matches!(
            self.slot,
            Slot::Failed(_) | Slot::GaveUp { .. } | Slot::AwaitingRestart { .. }
        );
        self.shown = false;
        self.dead_pane_focus = None;
        self.reconnect.connected();
        self.clear(cx);
    }

    /// Start a fresh connect unless the view is live, even while a connect is
    /// in flight: the window calls it once it has brought the orchestrator up
    /// (as the TUI re-attaches after its startup), so an attach that began
    /// before the session existed, and might give up on it, is replaced by
    /// one that finds it. Returns whether it kicked a connect.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.shown || matches!(self.slot, Slot::Live(_)) {
            return false;
        }
        self.reconnect.connected();
        self.restart.begin(false);
        self.start_connect(None, None, cx);
        true
    }

    /// Re-attach a shown view whose session wasn't there (idle, or a failed
    /// first connect), because something changed that may have brought one up:
    /// the TUI's `retry_if_stale` for the shown workspace
    /// (`rt-tui-idle-workspace-open`). Returns whether it kicked a connect.
    pub fn retry_if_idle(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.shown || !matches!(self.slot, Slot::Idle(_) | Slot::Failed(_)) {
            return false;
        }
        self.start_connect(None, None, cx);
        true
    }

    /// The pane is about to be torn down on purpose (the project is being
    /// quit): the exit that follows is not restarted.
    pub fn note_deliberate_close(&mut self) {
        self.restart.note_deliberate_close();
    }

    /// The deliberate teardown didn't happen after all (the quit failed).
    pub fn clear_deliberate_close(&mut self) {
        self.restart.clear_deliberate_close();
    }

    /// Where keyboard focus goes when the view takes it: the terminal, or the
    /// view itself until the pane is live (focus moves to the pane when it
    /// attaches).
    pub fn focus_target(&self, cx: &gpui::App) -> FocusHandle {
        match &self.slot {
            Slot::Live(pane) => pane.read(cx).focus_handle(cx),
            _ => self.focus_handle.clone(),
        }
    }

    /// Whether keyboard focus is in this view (its terminal or its notice).
    pub fn contains_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus_handle.contains_focused(window, cx)
            || self
                .pane()
                .is_some_and(|p| p.read(cx).focus_handle(cx).is_focused(window))
    }

    /// Drop whatever the slot holds and cancel in-flight work.
    fn clear(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self._connect = None;
        self._tick = None;
        self._pane_subscriptions.clear();
        self.slot = Slot::Empty;
        cx.notify();
    }

    /// Run a connect off the UI thread: the relaunch step first when asked
    /// (`Some(reopen)`), then the retried attach. `restart_attempt` tags a
    /// supervisor-kicked restart so the view shows its progress.
    fn start_connect(
        &mut self,
        relaunch: Option<bool>,
        restart_attempt: Option<u32>,
        cx: &mut Context<Self>,
    ) {
        self.generation += 1;
        let generation = self.generation;
        self._tick = None;
        self._pane_subscriptions.clear();
        // A reconnect attempt keeps showing "Reconnecting…" while it runs.
        if !self.reconnect.is_reconnecting() {
            self.slot = Slot::Connecting { restart_attempt };
        }
        cx.notify();

        let connector = self.deps.connector.clone();
        let relauncher = self.deps.relauncher.clone();
        let retry = self.deps.retry;
        let project = self.project.clone();
        let target = self.target.clone();
        let work = cx.background_executor().spawn(async move {
            // A relaunch failure is terminal and surfaced as the connect error,
            // so the view shows a clear message rather than retrying a relaunch
            // that can't happen.
            if let Some(reopen) = relaunch {
                if let Err(e) = relauncher.relaunch(&target, reopen) {
                    return Err(ConnectFailure::Message(format!(
                        "couldn't relaunch {}: {e}",
                        target.display()
                    )));
                }
            }
            connector.connect(&project, &target, retry)
        });
        let window = self.window;
        self._connect = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = cx.update_window(window, |_, window, cx| {
                this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.connected(result, window, cx);
                    }
                })
            });
        }));
    }

    /// A connect finished.
    fn connected(
        &mut self,
        result: Result<Connected, ConnectFailure>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._connect = None;
        let now = (self.deps.clock)();
        // The machine could not be reached: keep reconnecting rather than
        // counting a death (the session may well still be running there).
        if let Err(ConnectFailure::Unreachable(reason)) = &result {
            if self.reconnect.is_reconnecting() {
                self.reconnect.unreachable(reason, now);
            } else {
                self.reconnect.link_lost(reason, now);
                // A first attempt is immediate; it just failed, so back off.
                self.reconnect.take_due(now);
                self.reconnect.unreachable(reason, now);
            }
            self.slot = Slot::Reconnecting;
            self.schedule_reconnect(cx);
            cx.notify();
            return;
        }
        let was_reconnecting = self.reconnect.is_reconnecting();
        self.reconnect.connected();
        match result {
            Ok(connected) => {
                // Take focus if the view held it, or if the pane this one
                // replaces did and nothing else has taken it since (the user
                // may have moved to the sidebar during the restart).
                let had_focus = self.focus_handle.is_focused(window)
                    || self.dead_pane_focus.take().is_some_and(|old| {
                        window.focused(cx).is_none_or(|now| now == old)
                    });
                let pane = cx.new(|cx| TerminalPane::attached_to(connected, window, cx));
                let pane_focus = pane.read(cx).focus_handle(cx);
                self._pane_subscriptions = vec![
                    cx.subscribe_in(&pane, window, |this, pane, event: &PaneEvent, window, cx| {
                        // Note whether the pane holds focus before it may be
                        // replaced: its focus handle goes away with it.
                        let handle = pane.read(cx).focus_handle(cx);
                        this.dead_pane_focus = handle.is_focused(window).then_some(handle);
                        this.pane_event(event, cx)
                    }),
                    cx.on_focus_in(&pane_focus, window, |_, _, cx| {
                        cx.emit(SessionViewEvent::Focused)
                    }),
                ];
                self.slot = Slot::Live(pane);
                // Seen alive: the controller now adopts the pane for supervision.
                self.restart.alive(now);
                if had_focus {
                    window.focus(&pane_focus, cx);
                }
            }
            // The machine answered after a lost link and the session is gone
            // (it ended while we were cut off): that is a death.
            Err(ConnectFailure::Idle(_)) if was_reconnecting => {
                self.died(app_session::exit_line(None), cx);
            }
            Err(ConnectFailure::Idle(info)) => self.slot = Slot::Idle(info),
            // `retry_connect` only ever returns a terminal outcome; a transient
            // one here is treated as a plain failure rather than dropped.
            Err(
                ConnectFailure::Message(error)
                | ConnectFailure::Starting(error)
                | ConnectFailure::Awaiting(error)
                | ConnectFailure::Unreachable(error),
            ) => {
                self.slot = Slot::Failed(error.clone());
                if was_reconnecting {
                    self.died(error, cx);
                } else {
                    // A failed restart attempt is another death; a first-open
                    // failure is left showing (the controller says `Leave`).
                    self.act_on_death(error, cx);
                }
            }
        }
        cx.notify();
    }

    fn pane_event(&mut self, event: &PaneEvent, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Title(title) => {
                self.forward(shelbi_client::SessionEvent::Title(title.clone()))
            }
            PaneEvent::Exited(exited) => {
                self.forward(shelbi_client::SessionEvent::Exited(exited.clone()));
                self.live_pane_died(app_session::exit_line(Some(exited)), cx);
            }
            // A remote session's stream ending with no exit is the relay going
            // away under it (an SSH drop): reconnect.
            PaneEvent::Closed if self.remote_machine.is_some() => self.link_lost(cx),
            PaneEvent::Closed => self.live_pane_died(app_session::exit_line(None), cx),
        }
    }

    fn forward(&self, event: shelbi_client::SessionEvent) {
        if let Some(sink) = &self.sink {
            sink.forward(&self.view_name, &event);
        }
    }

    /// The live remote pane's connection dropped: it was alive until now (so
    /// the restart controller has adopted it), and the view now reconnects,
    /// with an immediate first attempt.
    fn link_lost(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.slot, Slot::Live(_)) {
            return;
        }
        let now = (self.deps.clock)();
        self.restart.alive(now);
        let machine = self.remote_machine.clone().unwrap_or_default();
        tracing::info!(project = %self.project, session = %self.target.display(), %machine, "lost the connection; reconnecting");
        self.reconnect.link_lost("The connection was lost.", now);
        self._pane_subscriptions.clear();
        self.slot = Slot::Reconnecting;
        self.schedule_reconnect(cx);
        cx.notify();
    }

    /// The attached session ended. It was alive until now (the TUI feeds the
    /// controller an alive tick every turn up to the death; one at the moment
    /// of death is the same observation), then dead.
    fn live_pane_died(&mut self, last_line: String, cx: &mut Context<Self>) {
        if !matches!(self.slot, Slot::Live(_)) {
            return;
        }
        self.restart.alive((self.deps.clock)());
        self.died(last_line, cx);
    }

    /// A pane that was up is gone: close it (the user shell) or ask the
    /// restart controller.
    fn died(&mut self, last_line: String, cx: &mut Context<Self>) {
        if self.on_death == OnDeath::Close {
            self.shown = false;
            self.reopen_on_show = false;
            self.dead_pane_focus = None;
            self.clear(cx);
            cx.emit(SessionViewEvent::Ended);
            return;
        }
        self.act_on_death(last_line, cx);
    }

    /// Ask the restart controller what to do about the dead pane and do it.
    fn act_on_death(&mut self, last_line: String, cx: &mut Context<Self>) {
        match self.restart.dead((self.deps.clock)()) {
            DeathAction::Restart { attempt } => {
                tracing::info!(
                    project = %self.project,
                    session = %self.target.display(),
                    attempt,
                    reason = %last_line,
                    "session exited; restarting"
                );
                // A daemon-supervised pane only re-attaches here; a content
                // session is re-ensured (the view's relauncher decides).
                self.start_connect(Some(false), Some(attempt), cx);
            }
            DeathAction::GiveUp => {
                tracing::warn!(project = %self.project, session = %self.target.display(), reason = %last_line, "restarts exhausted");
                self._tick = None;
                self.slot = Slot::GaveUp { last_line };
            }
            DeathAction::StandDown => {
                self._tick = None;
                self._pane_subscriptions.clear();
                self.slot = Slot::Empty;
            }
            DeathAction::Leave => {}
            DeathAction::Backoff { next_attempt } => {
                let already = matches!(
                    &self.slot,
                    Slot::AwaitingRestart { next_attempt: n, .. } if *n == next_attempt
                );
                if !already {
                    self._pane_subscriptions.clear();
                    self.slot = Slot::AwaitingRestart {
                        last_line,
                        next_attempt,
                    };
                }
                self.schedule_tick(cx);
            }
        }
        cx.notify();
    }

    /// While waiting out a backoff, re-ask the controller on a short timer.
    fn schedule_tick(&mut self, cx: &mut Context<Self>) {
        if self._tick.is_some() {
            return;
        }
        let timer = cx.background_executor().timer(RESTART_TICK);
        self._tick = Some(cx.spawn(async move |this, cx| {
            timer.await;
            let _ = this.update(cx, |this, cx| {
                this._tick = None;
                if let Slot::AwaitingRestart { last_line, .. } = &this.slot {
                    let last_line = last_line.clone();
                    this.act_on_death(last_line, cx);
                }
            });
        }));
    }

    /// Run the reconnect attempt when it is due: now, or after its backoff.
    fn schedule_reconnect(&mut self, cx: &mut Context<Self>) {
        let now = (self.deps.clock)();
        match self.reconnect.wait(now) {
            Some(wait) if wait.is_zero() => {
                if self.reconnect.take_due(now).is_some() {
                    self.start_connect(None, None, cx);
                }
            }
            Some(wait) => {
                let timer = cx.background_executor().timer(wait);
                self._tick = Some(cx.spawn(async move |this, cx| {
                    timer.await;
                    let _ = this.update(cx, |this, cx| {
                        this._tick = None;
                        if matches!(this.slot, Slot::Reconnecting) {
                            this.schedule_reconnect(cx);
                        }
                    });
                }));
            }
            None => {}
        }
    }
}

impl Focusable for SessionView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SessionView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let root = div()
            .id("session-view")
            .debug_selector(|| "session-view".into())
            .track_focus(&self.focus_handle)
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .size_full()
            .bg(theme.canvas);
        if let Slot::Live(pane) = &self.slot {
            return root.child(pane.clone());
        }
        if let Some(lines) = self.idle_lines() {
            // The TUI's idle placeholder: top-left, the name bold in the
            // foreground, the rest muted.
            return root.flex_col().p_4().gap_0p5().children(lines.into_iter().enumerate().map(
                |(i, line)| {
                    let row = div().min_h(gpui::px(18.0)).child(line);
                    if i == 0 {
                        row.font_weight(FontWeight::BOLD).text_color(theme.white)
                    } else {
                        row.text_color(theme.muted)
                    }
                },
            ));
        }
        root.items_center().justify_center().px_4().child(
            div()
                .debug_selector(|| "session-notice".into())
                // Long notices (a give-up with its last output) wrap inside
                // the main area.
                .max_w_full()
                .text_center()
                .text_color(theme.muted)
                .child(self.notice().unwrap_or_default()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Mutex;

    use futures::channel::mpsc::{unbounded, UnboundedSender};
    use gpui::{TestAppContext, VisualTestContext, WindowAppearance};
    use shelbi_app::supervision::{BASE_BACKOFF, MAX_RESTARTS_IN_WINDOW};
    use shelbi_client::SessionEvent;
    use shelbi_app::theme::ThemePreference;

    use crate::elements::terminal::session::SessionIo;

    /// What the fake session's input side saw.
    #[derive(Default)]
    struct Wire {
        input: Vec<Vec<u8>>,
        resizes: Vec<Size>,
    }

    struct FakeIo(Arc<Mutex<Wire>>);

    impl SessionIo for FakeIo {
        fn input(&self, bytes: Vec<u8>) {
            self.0.lock().unwrap().input.push(bytes);
        }
        fn paste(&self, text: String, _bracketed: bool) {
            self.0.lock().unwrap().input.push(text.into_bytes());
        }
        fn resize(&self, size: Size) {
            self.0.lock().unwrap().resizes.push(size);
        }
    }

    /// One scripted connect outcome.
    enum Outcome {
        /// Attach: the test keeps the sender to push output, an exit, or (by
        /// dropping it) a closed connection.
        Live(Size),
        Fail(&'static str),
        /// The machine couldn't be reached (a remote session's relay).
        Unreachable,
        /// A declared workspace with no session.
        Idle,
    }

    /// A fake session: each connect pops the next outcome (the last repeats).
    #[derive(Default)]
    struct FakeSession {
        script: Mutex<Vec<Outcome>>,
        connects: Mutex<u32>,
        /// The event senders of every attach, newest last.
        streams: Mutex<Vec<UnboundedSender<SessionEvent>>>,
        wire: Arc<Mutex<Wire>>,
        /// `reopen` flag of every relaunch step.
        relaunches: Mutex<Vec<bool>>,
    }

    impl FakeSession {
        fn with(script: Vec<Outcome>) -> Arc<Self> {
            Arc::new(FakeSession {
                script: Mutex::new(script),
                ..FakeSession::default()
            })
        }

        fn connects(&self) -> u32 {
            *self.connects.lock().unwrap()
        }

        /// Push an event into the newest attach.
        fn push(&self, ev: SessionEvent) {
            let streams = self.streams.lock().unwrap();
            streams.last().unwrap().unbounded_send(ev).unwrap();
        }

        fn exit(&self, code: i32) {
            self.push(SessionEvent::Exited(shelbi_proto::Exited {
                code: Some(code),
                signal: None,
                reason: None,
            }));
        }

        /// Drop the newest attach's stream: the connection closes with no exit.
        fn close(&self) {
            self.streams.lock().unwrap().pop();
        }
    }

    impl SessionConnector for FakeSession {
        fn connect(
            &self,
            project: &str,
            target: &SessionRef,
            _retry: RetryPolicy,
        ) -> Result<Connected, ConnectFailure> {
            *self.connects.lock().unwrap() += 1;
            let mut script = self.script.lock().unwrap();
            let outcome = if script.len() > 1 {
                script.remove(0)
            } else {
                match script.first() {
                    Some(Outcome::Live(s)) => Outcome::Live(*s),
                    Some(Outcome::Fail(m)) => Outcome::Fail(m),
                    Some(Outcome::Unreachable) => Outcome::Unreachable,
                    Some(Outcome::Idle) => Outcome::Idle,
                    None => Outcome::Fail("no script"),
                }
            };
            match outcome {
                Outcome::Live(size) => {
                    let (tx, rx) = unbounded();
                    // The attach replay: the scrollback the session holds.
                    tx.unbounded_send(SessionEvent::Resync {
                        seq: 0,
                        replay: b"earlier scrollback\r\n".to_vec(),
                    })
                    .unwrap();
                    self.streams.lock().unwrap().push(tx);
                    Ok(Connected {
                        link: Box::new(FakeIo(self.wire.clone())),
                        events: rx,
                        size,
                        name: app_session::discovery_name(project, target),
                    })
                }
                Outcome::Fail(m) => Err(ConnectFailure::Message(m.into())),
                Outcome::Unreachable => {
                    Err(ConnectFailure::Unreachable("`gpu` is unreachable".into()))
                }
                Outcome::Idle => Err(ConnectFailure::Idle(IdleInfo {
                    name: "alpha".into(),
                    machine: "hub".into(),
                    branch: Some("main".into()),
                })),
            }
        }
    }

    impl Relauncher for FakeSession {
        fn relaunch(&self, _target: &SessionRef, reopen: bool) -> Result<(), String> {
            self.relaunches.lock().unwrap().push(reopen);
            Ok(())
        }
    }

    struct Harness {
        chat: Entity<SessionView>,
        cx: VisualTestContext,
        session: Arc<FakeSession>,
        now: Rc<Cell<Instant>>,
    }

    impl Harness {
        fn phase(&mut self) -> SessionPhase {
            self.chat.read_with(&self.cx, |c, _| c.phase())
        }

        fn update<R>(&mut self, f: impl FnOnce(&mut SessionView, &mut Context<SessionView>) -> R) -> R {
            let r = self.chat.update(&mut self.cx, f);
            self.cx.run_until_parked();
            r
        }

        fn settle(&mut self) {
            self.cx.run_until_parked();
        }

        /// Move both clocks: the controller's and gpui's timers.
        fn advance(&mut self, by: Duration) {
            self.now.set(self.now.get() + by);
            self.cx.executor().advance_clock(by);
            self.cx.run_until_parked();
        }

        /// Advance in restart ticks until the phase stops being a backoff
        /// wait for `attempt` (or the budget runs out).
        fn wait_out_backoff(&mut self) {
            for _ in 0..2000 {
                match self.phase() {
                    SessionPhase::Restarting(..) if !self.connecting() => self.advance(RESTART_TICK),
                    _ => return,
                }
            }
            panic!("the backoff never ended");
        }

        fn connecting(&mut self) -> bool {
            self.chat
                .read_with(&self.cx, |c, _| matches!(c.slot, Slot::Connecting { .. }))
        }
    }

    /// Every event the view emitted, oldest first.
    fn record_events(h: &mut Harness) -> Rc<std::cell::RefCell<Vec<SessionViewEvent>>> {
        let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = seen.clone();
        h.cx.update(|_, cx| {
            cx.subscribe(&h.chat, move |_, ev: &SessionViewEvent, _| sink.borrow_mut().push(ev.clone()))
                .detach();
        });
        seen
    }

    fn open(cx: &mut TestAppContext, script: Vec<Outcome>) -> Harness {
        cx.update(|cx| {
            cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
        });
        let session = FakeSession::with(script);
        let now = Rc::new(Cell::new(Instant::now()));
        let deps = {
            let now = now.clone();
            SessionDeps {
                connector: session.clone(),
                relauncher: session.clone(),
                retry: RetryPolicy::default(),
                reconnect: ReconnectPolicy {
                    first_backoff: Duration::from_secs(1),
                    max_backoff: Duration::from_secs(4),
                },
                clock: Rc::new(move || now.get()),
            }
        };
        let deps = Cell::new(Some(deps));
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let handle = window.window_handle();
                cx.new(|cx| SessionView::orchestrator("demo", handle, deps.take().unwrap(), None, cx))
            })
            .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let chat = window.root(&mut vcx).unwrap();
        vcx.run_until_parked();
        Harness {
            chat,
            cx: vcx,
            session,
            now,
        }
    }

    #[gpui::test]
    fn showing_attaches_with_replay_and_hiding_detaches(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(100, 30))]);
        assert_eq!(h.phase(), SessionPhase::Empty, "nothing connects until shown");
        assert_eq!(h.session.connects(), 0);

        h.update(|c, cx| c.show(cx));
        assert_eq!(h.phase(), SessionPhase::Live);
        assert_eq!(h.session.connects(), 1);
        // The attach replay reached the terminal.
        let text = h.chat.read_with(&h.cx, |c, cx| {
            c.pane()
                .unwrap()
                .read(cx)
                .terminal_text()
        });
        assert!(text.contains("earlier scrollback"), "{text:?}");

        // Showing again while live is a no-op.
        h.update(|c, cx| c.show(cx));
        assert_eq!(h.session.connects(), 1);

        // Hiding drops the pane (and with it the connection).
        let weak = h.chat.read_with(&h.cx, |c, _| c.pane().unwrap().downgrade());
        h.update(|c, cx| c.hide(cx));
        assert_eq!(h.phase(), SessionPhase::Empty);
        assert!(weak.upgrade().is_none(), "the pane is released on hide");

        // Showing again re-attaches with a fresh replay.
        h.update(|c, cx| c.show(cx));
        assert_eq!(h.phase(), SessionPhase::Live);
        assert_eq!(h.session.connects(), 2);
        assert!(
            h.session.relaunches.lock().unwrap().is_empty(),
            "a plain show runs no relaunch step"
        );
    }

    #[gpui::test]
    fn typing_reaches_the_session(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        h.update(|c, cx| c.show(cx));
        h.cx.update(|window, cx| {
            let target = h.chat.read(cx).focus_target(cx);
            window.focus(&target, cx);
        });
        h.settle();
        h.cx.simulate_keystrokes("h i enter");
        h.cx.simulate_keystrokes("shift-enter");
        let input = h.session.wire.lock().unwrap().input.concat();
        assert_eq!(input, b"hi\r\r", "legacy encoding until the program enables kitty");
        // Under the kitty keyboard protocol (Claude Code turns it on), Shift+Enter
        // is the CSI-u form Claude Code reads as a newline.
        h.session.push(SessionEvent::Output {
            seq: 1,
            data: b"\x1b[>1u".to_vec(),
        });
        h.settle();
        h.session.wire.lock().unwrap().input.clear();
        h.cx.simulate_keystrokes("shift-enter");
        assert_eq!(h.session.wire.lock().unwrap().input.concat(), b"\x1b[13;2u");
    }

    #[gpui::test]
    fn an_exit_restarts_and_reattaches(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        h.update(|c, cx| c.show(cx));
        assert_eq!(h.phase(), SessionPhase::Live);

        // The orchestrator exits (a crash, or `shelbi reload` relaunching it):
        // the first restart re-attaches at once.
        h.session.exit(1);
        h.settle();
        assert_eq!(h.phase(), SessionPhase::Live, "reattached on its own");
        assert_eq!(h.session.connects(), 2);
        assert_eq!(
            *h.session.relaunches.lock().unwrap(),
            vec![false],
            "an automatic restart only re-attaches (the daemon relaunches the orchestrator)"
        );

        // A connection that drops with no exit (the session process went away)
        // counts as a death too; within the backoff it waits, then reattaches.
        h.session.close();
        h.settle();
        assert_eq!(
            h.phase(),
            SessionPhase::Restarting(2, MAX_RESTARTS_IN_WINDOW as u32),
            "waits out the backoff, showing the next attempt"
        );
        assert_eq!(
            h.chat.read_with(&h.cx, |c, _| c.notice()).as_deref(),
            Some("Session exited — restarting (2/5)…")
        );
        h.wait_out_backoff();
        assert_eq!(h.phase(), SessionPhase::Live);
        assert_eq!(h.session.connects(), 3);
    }

    #[gpui::test]
    fn a_reattached_pane_keeps_keyboard_focus(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        h.update(|c, cx| c.show(cx));
        h.cx.update(|window, cx| {
            let target = h.chat.read(cx).focus_target(cx);
            window.focus(&target, cx);
        });
        h.settle();
        h.session.exit(1);
        h.settle();
        assert_eq!(h.phase(), SessionPhase::Live);
        let focused = h.cx.update(|window, cx| {
            h.chat.read(cx).pane().unwrap().read(cx).focus_handle(cx).is_focused(window)
        });
        assert!(focused, "the replacement pane takes the focus its predecessor had");
        h.session.wire.lock().unwrap().input.clear();
        h.cx.simulate_keystrokes("x");
        assert_eq!(h.session.wire.lock().unwrap().input.concat(), b"x");
    }

    #[gpui::test]
    fn a_relaunch_gap_reattaches_after_failed_attempts(cx: &mut TestAppContext) {
        // The relaunched orchestrator takes a while: the restart's connect
        // fails, the view keeps restarting on the backoff, and attaches as soon
        // as the new session is up.
        let mut h = open(
            cx,
            vec![
                Outcome::Live(Size::new(80, 24)),
                Outcome::Fail("no live session `demo/orch`"),
                Outcome::Live(Size::new(80, 24)),
            ],
        );
        h.update(|c, cx| c.show(cx));
        h.session.exit(0);
        h.settle();
        assert!(matches!(h.phase(), SessionPhase::Restarting(2, _)), "{:?}", h.phase());
        h.wait_out_backoff();
        assert_eq!(h.phase(), SessionPhase::Live);
        assert_eq!(h.session.connects(), 3);
    }

    #[gpui::test]
    fn a_crash_loop_gives_up_and_reselecting_relaunches(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        h.update(|c, cx| c.show(cx));
        // Die right after every reattach, past each backoff, until the cap.
        for _ in 0..MAX_RESTARTS_IN_WINDOW {
            h.session.exit(1);
            h.settle();
            h.wait_out_backoff();
            assert_eq!(h.phase(), SessionPhase::Live);
        }
        h.session.exit(1);
        h.settle();
        h.wait_out_backoff();
        assert_eq!(h.phase(), SessionPhase::GaveUp("exited with status 1".into()));
        assert_eq!(
            h.chat.read_with(&h.cx, |c, _| c.notice()).as_deref(),
            Some("Stopped after 5 restarts — select it again to relaunch. Last output: exited with status 1")
        );
        let connects = h.session.connects();
        h.advance(BASE_BACKOFF * 64);
        assert_eq!(h.session.connects(), connects, "a given-up pane stays down");

        // Selecting it again resets the budget and runs a reopen relaunch.
        h.update(|c, cx| c.show(cx));
        assert_eq!(h.phase(), SessionPhase::Live);
        assert_eq!(h.session.relaunches.lock().unwrap().last(), Some(&true));
    }

    #[gpui::test]
    fn a_dead_pane_hidden_and_shown_again_is_reopened(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Fail("no live session `demo/orch`")]);
        h.update(|c, cx| c.show(cx));
        assert_eq!(
            h.phase(),
            SessionPhase::Failed("no live session `demo/orch`".into()),
            "a first-open failure is shown, not restarted"
        );
        h.advance(BASE_BACKOFF * 4);
        assert_eq!(h.session.connects(), 1);

        h.update(|c, cx| c.hide(cx));
        h.update(|c, cx| c.show(cx));
        // A reopen relaunch, then (the orchestrator is still not there) the
        // re-armed budget restarts it on its own, as the TUI does.
        assert_eq!(h.session.relaunches.lock().unwrap().first(), Some(&true));
        assert_eq!(*h.session.relaunches.lock().unwrap(), vec![true, false]);
        assert!(matches!(h.phase(), SessionPhase::Restarting(2, _)), "{:?}", h.phase());
    }

    #[gpui::test]
    fn a_deliberate_close_is_not_restarted(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        h.update(|c, cx| c.show(cx));
        h.update(|c, _| c.note_deliberate_close());
        h.session.exit(0);
        h.settle();
        assert_eq!(h.phase(), SessionPhase::Empty, "stands down");
        h.advance(BASE_BACKOFF * 4);
        assert_eq!(h.session.connects(), 1, "no reattach after a deliberate close");
    }

    #[gpui::test]
    fn a_stale_failure_retries_when_asked(cx: &mut TestAppContext) {
        let mut h = open(
            cx,
            vec![
                Outcome::Fail("no live session `demo/orch`"),
                Outcome::Live(Size::new(80, 24)),
            ],
        );
        h.update(|c, cx| c.show(cx));
        assert!(matches!(h.phase(), SessionPhase::Failed(_)));
        // The window brought the orchestrator up: retry without a re-select.
        assert!(h.update(|c, cx| c.reconnect(cx)));
        assert_eq!(h.phase(), SessionPhase::Live);
    }

    #[gpui::test]
    fn reconnect_replaces_an_in_flight_connect(cx: &mut TestAppContext) {
        let mut h = open(
            cx,
            vec![
                Outcome::Fail("session `demo/orch` is starting (socket not up yet)"),
                Outcome::Live(Size::new(80, 24)),
            ],
        );
        // The window brings the orchestrator up while the first connect (the
        // one that would give up) is still in flight.
        h.chat.update(&mut h.cx, |c, cx| {
            c.show(cx);
            assert!(c.reconnect(cx));
        });
        h.settle();
        assert_eq!(h.phase(), SessionPhase::Live, "the stale connect's failure is dropped");
        assert!(!h.update(|c, cx| c.reconnect(cx)), "a live view is left alone");
        h.update(|c, cx| c.hide(cx));
        assert!(!h.update(|c, cx| c.reconnect(cx)), "a hidden view is left alone");
    }

    #[gpui::test]
    fn a_result_for_a_hidden_view_is_dropped(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        // Show and hide before the connect lands.
        h.chat.update(&mut h.cx, |c, cx| {
            c.show(cx);
            c.hide(cx);
        });
        h.settle();
        assert_eq!(h.phase(), SessionPhase::Empty);
    }

    #[gpui::test]
    fn the_pane_reports_its_size_to_the_session(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(120, 40))]);
        h.update(|c, cx| c.show(cx));
        // Lay the view out: the element measures its bounds and reports the
        // grid it fits (the session then follows the most recently active
        // client, so a TUI at another size reflows when either one types).
        h.cx.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
        h.settle();
        let resizes = h.session.wire.lock().unwrap().resizes.clone();
        assert!(!resizes.is_empty(), "the first size is reported at once");
        assert_ne!(resizes[0], Size::new(120, 40), "the window's own size, not the session's");
    }

    #[gpui::test]
    fn a_dropped_remote_connection_reconnects_without_spending_a_restart(cx: &mut TestAppContext) {
        // A remote workspace's agent: Live, then the SSH connection under the
        // relay drops (the stream ends with no exit). The machine is
        // unreachable for two attempts, then comes back.
        let mut h = open(
            cx,
            vec![
                Outcome::Live(Size::new(80, 24)),
                Outcome::Unreachable,
                Outcome::Unreachable,
                Outcome::Live(Size::new(80, 24)),
            ],
        );
        h.update(|c, _| c.set_remote_machine(Some("gpu".into())));
        h.update(|c, cx| c.show(cx));
        assert_eq!(h.phase(), SessionPhase::Live);

        h.session.close();
        h.settle();
        // The immediate first attempt ran and found the machine unreachable;
        // the second waits out the backoff.
        assert_eq!(h.session.connects(), 2);
        assert_eq!(h.phase(), SessionPhase::Reconnecting(2));
        assert_eq!(
            h.chat.read_with(&h.cx, |c, _| c.notice()).as_deref(),
            Some("Reconnecting to gpu… (attempt 2) `gpu` is unreachable")
        );
        h.advance(Duration::from_millis(900));
        assert_eq!(h.session.connects(), 2, "not inside the backoff");
        h.advance(Duration::from_millis(100));
        assert_eq!(h.session.connects(), 3);
        assert_eq!(h.phase(), SessionPhase::Reconnecting(3), "the backoff doubles");
        h.advance(Duration::from_secs(2));
        assert_eq!(h.phase(), SessionPhase::Live, "back once the machine answers");
        assert_eq!(h.session.connects(), 4);
        assert!(
            h.session.relaunches.lock().unwrap().is_empty(),
            "a reconnect never relaunches the session"
        );

        // The next drop starts over at an immediate attempt, and the restart
        // budget is untouched: an exit still restarts at once.
        h.session.exit(1);
        h.settle();
        assert_eq!(h.phase(), SessionPhase::Live);
        assert_eq!(*h.session.relaunches.lock().unwrap(), vec![false]);
    }

    #[gpui::test]
    fn a_remote_machine_unreachable_at_open_keeps_reconnecting(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Unreachable, Outcome::Live(Size::new(80, 24))]);
        h.update(|c, _| c.set_remote_machine(Some("gpu".into())));
        h.update(|c, cx| c.show(cx));
        assert_eq!(h.phase(), SessionPhase::Reconnecting(2));
        h.advance(Duration::from_secs(1));
        assert_eq!(h.phase(), SessionPhase::Live);
    }

    #[gpui::test]
    fn a_reconnect_that_finds_the_session_gone_is_a_death(cx: &mut TestAppContext) {
        let mut h = open(
            cx,
            vec![
                Outcome::Live(Size::new(80, 24)),
                Outcome::Fail("no live session `demo/ws/alpha`"),
                Outcome::Live(Size::new(80, 24)),
            ],
        );
        h.update(|c, _| c.set_remote_machine(Some("gpu".into())));
        h.update(|c, cx| c.show(cx));
        h.session.close();
        h.settle();
        // The machine answered, the session had ended: the restart controller
        // takes over and its first restart reattaches.
        assert_eq!(h.phase(), SessionPhase::Live);
        assert_eq!(h.session.connects(), 3);
        assert_eq!(*h.session.relaunches.lock().unwrap(), vec![false]);
    }

    #[gpui::test]
    fn hiding_a_reconnecting_view_stops_reconnecting(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24)), Outcome::Unreachable]);
        h.update(|c, _| c.set_remote_machine(Some("gpu".into())));
        h.update(|c, cx| c.show(cx));
        h.session.close();
        h.settle();
        assert!(matches!(h.phase(), SessionPhase::Reconnecting(_)));
        h.update(|c, cx| c.hide(cx));
        let connects = h.session.connects();
        h.advance(Duration::from_secs(30));
        assert_eq!(h.session.connects(), connects);
        assert_eq!(h.phase(), SessionPhase::Empty);
    }

    #[gpui::test]
    fn a_local_connection_drop_is_still_a_death(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        h.update(|c, cx| c.show(cx));
        h.session.close();
        h.settle();
        assert_eq!(h.phase(), SessionPhase::Live, "restarted, not reconnected");
        assert_eq!(*h.session.relaunches.lock().unwrap(), vec![false]);
    }

    #[gpui::test]
    fn a_close_on_death_pane_ends_instead_of_restarting(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        let events = record_events(&mut h);
        h.update(|c, _| c.set_on_death(OnDeath::Close));
        h.update(|c, cx| c.show(cx));
        h.session.exit(0);
        h.settle();
        assert_eq!(h.phase(), SessionPhase::Empty);
        assert!(!h.chat.read_with(&h.cx, |c, _| c.is_shown()));
        assert!(events.borrow().contains(&SessionViewEvent::Ended));
        h.advance(BASE_BACKOFF * 4);
        assert_eq!(h.session.connects(), 1, "never restarted");
    }

    #[gpui::test]
    fn showing_another_target_starts_fresh(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        h.update(|c, cx| c.show(cx));
        let diff = SessionRef::WorkspaceContent {
            workspace: "alpha".into(),
            role: "diff".into(),
        };
        h.update(|c, cx| c.show_target(diff.clone(), cx));
        assert_eq!(h.phase(), SessionPhase::Live);
        assert_eq!(h.session.connects(), 2);
        assert_eq!(h.chat.read_with(&h.cx, |c, _| c.target().clone()), diff);
        // Re-selecting the same target while live is a no-op.
        h.update(|c, cx| c.show_target(diff.clone(), cx));
        assert_eq!(h.session.connects(), 2);
    }

    #[gpui::test]
    fn an_idle_workspace_shows_the_placeholder_and_retries_on_change(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Idle, Outcome::Live(Size::new(80, 24))]);
        h.update(|c, _| c.set_palette_chord("⌃P"));
        h.update(|c, cx| c.show(cx));
        assert_eq!(h.phase(), SessionPhase::Idle);
        let lines = h.chat.read_with(&h.cx, |c, _| c.idle_lines()).unwrap();
        assert_eq!(lines[0], "alpha");
        assert_eq!(lines[1], "hub · main");
        assert!(lines.iter().any(|l| l.contains("(⌃P)")), "{lines:?}");
        // Nothing launches on its own.
        h.advance(BASE_BACKOFF * 4);
        assert_eq!(h.session.connects(), 1);
        assert!(h.session.relaunches.lock().unwrap().is_empty());
        // The workspace changed (a dispatch started a session): re-attach.
        assert!(h.update(|c, cx| c.retry_if_idle(cx)));
        assert_eq!(h.phase(), SessionPhase::Live);
        assert!(!h.update(|c, cx| c.retry_if_idle(cx)), "a live view is left alone");
    }

    #[gpui::test]
    fn a_drop_pastes_its_paths_into_the_live_session(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        h.update(|c, cx| c.show(cx));
        h.cx.update(|window, cx| window.draw(cx).clear(cx));
        h.settle();
        let center = h.cx.update(|window, _| {
            let size = window.viewport_size();
            gpui::point(size.width / 2.0, size.height / 2.0)
        });
        let paths = gpui::ExternalPaths(
            vec![std::path::PathBuf::from("/tmp/Screen Shot.png")].into(),
        );
        h.cx.simulate_event(gpui::FileDropEvent::Entered {
            position: center,
            paths,
        });
        h.cx.simulate_event(gpui::FileDropEvent::Submit { position: center });
        h.settle();
        let input = h.session.wire.lock().unwrap().input.concat();
        assert_eq!(String::from_utf8(input).unwrap(), "/tmp/Screen\\ Shot.png");
        let focused = h.cx.update(|window, cx| {
            h.chat.read(cx).pane().unwrap().read(cx).focus_handle(cx).is_focused(window)
        });
        assert!(focused, "the drop focuses the terminal");
    }
}
