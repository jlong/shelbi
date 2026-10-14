//! The Chat view: the project's orchestrator session (`<project>/orch`) as a
//! full-height terminal in the main area. This is the view the user lives in.
//!
//! It is the desktop twin of the TUI's `SessionManager` for the main area and
//! keeps its behavior, built from the same toolkit-free pieces in
//! `shelbi_app::session`:
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
//!   after a deliberate teardown (quitting the project). The orchestrator is
//!   daemon-supervised, so an automatic restart only re-attaches; re-opening a
//!   given-up pane drops the daemon's relaunch marker
//!   ([`shelbi_app::session::PersistentRelauncher`]).
//! - **Sizing** is the terminal element's: it reports its size (debounced) and
//!   the session follows the most recently active client, so a TUI attached at
//!   another size reflows whenever either client types.
//!
//! The connect seam ([`ChatConnector`]), the relauncher and the clock are
//! injected, so the attach state machine is tested in a gpui test context with
//! a fake session (below).

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, AnyWindowHandle, Context, Entity, EventEmitter, FocusHandle, Focusable,
    Subscription, Task, Window,
};
use shelbi_app::session::{
    self as app_session, ConnectFailure, IdleInfo, PersistentRelauncher, Relauncher,
    RestartController, RetryPolicy, SessionRef,
};
use shelbi_app::session::DeathAction;
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
pub trait ChatConnector: Send + Sync {
    /// Connect to `target` in `project`, retrying transient outcomes on
    /// `retry`. Blocking: the view runs it on the background executor.
    fn connect(
        &self,
        project: &str,
        target: &SessionRef,
        retry: RetryPolicy,
    ) -> Result<Connected, ConnectFailure>;
}

/// The production connector: `shelbi_app::session`'s discovery + attach with
/// replay, retried, then wired onto the pane's threads.
pub struct LiveConnector;

struct AppConnector;

impl app_session::Connector for AppConnector {
    fn connect(
        &self,
        project: &str,
        target: &SessionRef,
    ) -> Result<app_session::Connected, ConnectFailure> {
        app_session::connect(project, target, |project, name| {
            match shelbi_orchestrator::workspace::resolve_idle_workspace(project, name) {
                Some(id) => IdleInfo {
                    name: name.to_string(),
                    machine: id.machine,
                    branch: id.branch,
                },
                None => IdleInfo::named(name),
            }
        })
    }
}

impl ChatConnector for LiveConnector {
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
pub struct ChatDeps {
    pub connector: Arc<dyn ChatConnector>,
    pub relauncher: Arc<dyn Relauncher>,
    pub retry: RetryPolicy,
    /// The supervision clock (the restart backoff and recovery windows).
    pub clock: Rc<dyn Fn() -> Instant>,
}

impl ChatDeps {
    /// The real session, the orchestrator's daemon-side relaunch markers, and
    /// the wall clock.
    pub fn live(project: &str) -> Self {
        Self {
            connector: Arc::new(LiveConnector),
            relauncher: Arc::new(PersistentRelauncher {
                project: project.to_string(),
            }),
            retry: RetryPolicy::default(),
            clock: Rc::new(Instant::now),
        }
    }
}

/// What the view tells its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatEvent {
    /// The terminal took keyboard focus (a click, or the view being shown):
    /// the window's focus model moves to the main area.
    Focused,
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
}

/// The view's state without its payloads (the tests' view of the machine).
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatPhase {
    Empty,
    Connecting,
    Live,
    Idle,
    Failed(String),
    /// `(attempt, max)`: a restart in flight or waiting out its backoff.
    Restarting(u32, u32),
    GaveUp(String),
}

/// The orchestrator chat for one project window.
pub struct ChatView {
    project: String,
    target: SessionRef,
    /// The `View::Session` name this view is shown for (`orch`), which also
    /// keys its title in the window's session notices.
    view_name: String,
    window: AnyWindowHandle,
    deps: ChatDeps,
    slot: Slot,
    restart: RestartController,
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

impl EventEmitter<ChatEvent> for ChatView {}

impl ChatView {
    /// The orchestrator chat for `project`, in `window`. Nothing connects until
    /// [`show`](Self::show).
    pub fn new(
        project: &str,
        window: AnyWindowHandle,
        deps: ChatDeps,
        sink: Option<SessionSink>,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            project: project.to_string(),
            target: SessionRef::Orchestrator,
            view_name: "orch".into(),
            window,
            deps,
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

    /// Whether the window shows this view.
    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The view's state.
    #[cfg(test)]
    pub fn phase(&self) -> ChatPhase {
        let max = RestartController::max_restarts();
        match &self.slot {
            Slot::Empty => ChatPhase::Empty,
            Slot::Connecting {
                restart_attempt: Some(a),
            } => ChatPhase::Restarting(*a, max),
            Slot::Connecting { .. } => ChatPhase::Connecting,
            Slot::Live(_) => ChatPhase::Live,
            Slot::Idle(_) => ChatPhase::Idle,
            Slot::Failed(e) => ChatPhase::Failed(e.clone()),
            Slot::AwaitingRestart { next_attempt, .. } => ChatPhase::Restarting(*next_attempt, max),
            Slot::GaveUp { last_line } => ChatPhase::GaveUp(last_line.clone()),
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
    /// TUI words it.
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
        })
    }

    /// Show the view: attach (with replay) unless already connecting or live.
    /// Showing it again while it is failed, given up or mid-restart re-opens it:
    /// the restart budget resets and the orchestrator's daemon budget is reset
    /// too (a reopen relaunch), as re-selecting a pane does in the TUI.
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if self.shown && matches!(self.slot, Slot::Connecting { .. } | Slot::Live(_)) {
            return;
        }
        let dead = matches!(
            self.slot,
            Slot::Failed(_) | Slot::GaveUp { .. } | Slot::AwaitingRestart { .. }
        );
        let reopen = (self.shown && dead) || (!self.shown && self.reopen_on_show);
        self.shown = true;
        self.reopen_on_show = false;
        self.restart.begin(reopen);
        self.start_connect(reopen.then_some(true), None, cx);
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
        self.restart.begin(false);
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
        self.slot = Slot::Connecting { restart_attempt };
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
                    cx.on_focus_in(&pane_focus, window, |_, _, cx| cx.emit(ChatEvent::Focused)),
                ];
                self.slot = Slot::Live(pane);
                // Seen alive: the controller now adopts the pane for supervision.
                self.restart.alive((self.deps.clock)());
                if had_focus {
                    window.focus(&pane_focus, cx);
                }
            }
            Err(ConnectFailure::Idle(info)) => self.slot = Slot::Idle(info),
            // `retry_connect` only ever returns a terminal outcome; a transient
            // one here is treated as a plain failure rather than dropped.
            Err(
                ConnectFailure::Message(error)
                | ConnectFailure::Starting(error)
                | ConnectFailure::Awaiting(error),
            ) => {
                self.slot = Slot::Failed(error.clone());
                // A failed restart attempt is another death; a first-open
                // failure is left showing (the controller says `Leave`).
                self.act_on_death(error, cx);
            }
        }
        cx.notify();
    }

    fn pane_event(&mut self, event: &PaneEvent, cx: &mut Context<Self>) {
        match event {
            PaneEvent::Title(title) => self.forward(shelbi_client::SessionEvent::Title(title.clone())),
            PaneEvent::Exited(exited) => {
                self.forward(shelbi_client::SessionEvent::Exited(exited.clone()));
                self.live_pane_died(app_session::exit_line(Some(exited)), cx);
            }
            PaneEvent::Closed => self.live_pane_died(app_session::exit_line(None), cx),
        }
    }

    fn forward(&self, event: shelbi_client::SessionEvent) {
        if let Some(sink) = &self.sink {
            sink.forward(&self.view_name, &event);
        }
    }

    /// The attached session ended. It was alive until now (the TUI feeds the
    /// controller an alive tick every turn up to the death; one at the moment
    /// of death is the same observation), then dead.
    fn live_pane_died(&mut self, last_line: String, cx: &mut Context<Self>) {
        if !matches!(self.slot, Slot::Live(_)) {
            return;
        }
        self.restart.alive((self.deps.clock)());
        self.act_on_death(last_line, cx);
    }

    /// Ask the restart controller what to do about the dead pane and do it.
    fn act_on_death(&mut self, last_line: String, cx: &mut Context<Self>) {
        match self.restart.dead((self.deps.clock)()) {
            DeathAction::Restart { attempt } => {
                tracing::info!(
                    project = %self.project,
                    attempt,
                    reason = %last_line,
                    "orchestrator session exited; reattaching"
                );
                // The daemon relaunches the orchestrator; an automatic restart
                // only re-attaches (a non-reopen relaunch step).
                self.start_connect(Some(false), Some(attempt), cx);
            }
            DeathAction::GiveUp => {
                tracing::warn!(project = %self.project, reason = %last_line, "orchestrator restarts exhausted");
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
}

impl Focusable for ChatView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for ChatView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let root = div()
            .id("chat-view")
            .debug_selector(|| "chat-view".into())
            .track_focus(&self.focus_handle)
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .size_full()
            .bg(theme.canvas);
        match (&self.slot, self.notice()) {
            (Slot::Live(pane), _) => root.child(pane.clone()),
            (_, notice) => root
                .items_center()
                .justify_center()
                .px_4()
                .child(
                    div()
                        .debug_selector(|| "chat-notice".into())
                        // Long notices (a give-up with its last output) wrap
                        // inside the main area.
                        .max_w_full()
                        .text_center()
                        .text_color(theme.muted)
                        .child(notice.unwrap_or_default()),
                ),
        }
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

    impl ChatConnector for FakeSession {
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
        chat: Entity<ChatView>,
        cx: VisualTestContext,
        session: Arc<FakeSession>,
        now: Rc<Cell<Instant>>,
    }

    impl Harness {
        fn phase(&mut self) -> ChatPhase {
            self.chat.read_with(&self.cx, |c, _| c.phase())
        }

        fn update<R>(&mut self, f: impl FnOnce(&mut ChatView, &mut Context<ChatView>) -> R) -> R {
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
                    ChatPhase::Restarting(..) if !self.connecting() => self.advance(RESTART_TICK),
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

    fn open(cx: &mut TestAppContext, script: Vec<Outcome>) -> Harness {
        cx.update(|cx| {
            cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
        });
        let session = FakeSession::with(script);
        let now = Rc::new(Cell::new(Instant::now()));
        let deps = {
            let now = now.clone();
            ChatDeps {
                connector: session.clone(),
                relauncher: session.clone(),
                retry: RetryPolicy::default(),
                clock: Rc::new(move || now.get()),
            }
        };
        let deps = Cell::new(Some(deps));
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let handle = window.window_handle();
                cx.new(|cx| ChatView::new("demo", handle, deps.take().unwrap(), None, cx))
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
        assert_eq!(h.phase(), ChatPhase::Empty, "nothing connects until shown");
        assert_eq!(h.session.connects(), 0);

        h.update(|c, cx| c.show(cx));
        assert_eq!(h.phase(), ChatPhase::Live);
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
        assert_eq!(h.phase(), ChatPhase::Empty);
        assert!(weak.upgrade().is_none(), "the pane is released on hide");

        // Showing again re-attaches with a fresh replay.
        h.update(|c, cx| c.show(cx));
        assert_eq!(h.phase(), ChatPhase::Live);
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
        assert_eq!(h.phase(), ChatPhase::Live);

        // The orchestrator exits (a crash, or `shelbi reload` relaunching it):
        // the first restart re-attaches at once.
        h.session.exit(1);
        h.settle();
        assert_eq!(h.phase(), ChatPhase::Live, "reattached on its own");
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
            ChatPhase::Restarting(2, MAX_RESTARTS_IN_WINDOW as u32),
            "waits out the backoff, showing the next attempt"
        );
        assert_eq!(
            h.chat.read_with(&h.cx, |c, _| c.notice()).as_deref(),
            Some("Session exited — restarting (2/5)…")
        );
        h.wait_out_backoff();
        assert_eq!(h.phase(), ChatPhase::Live);
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
        assert_eq!(h.phase(), ChatPhase::Live);
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
        assert!(matches!(h.phase(), ChatPhase::Restarting(2, _)), "{:?}", h.phase());
        h.wait_out_backoff();
        assert_eq!(h.phase(), ChatPhase::Live);
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
            assert_eq!(h.phase(), ChatPhase::Live);
        }
        h.session.exit(1);
        h.settle();
        h.wait_out_backoff();
        assert_eq!(h.phase(), ChatPhase::GaveUp("exited with status 1".into()));
        assert_eq!(
            h.chat.read_with(&h.cx, |c, _| c.notice()).as_deref(),
            Some("Stopped after 5 restarts — select it again to relaunch. Last output: exited with status 1")
        );
        let connects = h.session.connects();
        h.advance(BASE_BACKOFF * 64);
        assert_eq!(h.session.connects(), connects, "a given-up pane stays down");

        // Selecting it again resets the budget and runs a reopen relaunch.
        h.update(|c, cx| c.show(cx));
        assert_eq!(h.phase(), ChatPhase::Live);
        assert_eq!(h.session.relaunches.lock().unwrap().last(), Some(&true));
    }

    #[gpui::test]
    fn a_dead_pane_hidden_and_shown_again_is_reopened(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Fail("no live session `demo/orch`")]);
        h.update(|c, cx| c.show(cx));
        assert_eq!(
            h.phase(),
            ChatPhase::Failed("no live session `demo/orch`".into()),
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
        assert!(matches!(h.phase(), ChatPhase::Restarting(2, _)), "{:?}", h.phase());
    }

    #[gpui::test]
    fn a_deliberate_close_is_not_restarted(cx: &mut TestAppContext) {
        let mut h = open(cx, vec![Outcome::Live(Size::new(80, 24))]);
        h.update(|c, cx| c.show(cx));
        h.update(|c, _| c.note_deliberate_close());
        h.session.exit(0);
        h.settle();
        assert_eq!(h.phase(), ChatPhase::Empty, "stands down");
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
        assert!(matches!(h.phase(), ChatPhase::Failed(_)));
        // The window brought the orchestrator up: retry without a re-select.
        assert!(h.update(|c, cx| c.reconnect(cx)));
        assert_eq!(h.phase(), ChatPhase::Live);
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
        assert_eq!(h.phase(), ChatPhase::Live, "the stale connect's failure is dropped");
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
        assert_eq!(h.phase(), ChatPhase::Empty);
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
}
