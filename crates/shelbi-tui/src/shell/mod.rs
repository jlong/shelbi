//! The single-process TUI shell (removing-tmux Phase 4b).
//!
//! With the `session_backend` dev flag on, `shelbi` runs this instead of
//! `exec tmux attach`: one ratatui program that owns the whole screen, a
//! sidebar on the left and a main area on the right. The main area shows a
//! session through a [`terminal_view::TerminalPane`] (the orchestrator chat and
//! each workspace agent), or a placeholder for the native views that land in
//! `rt-tui-native-views`.
//!
//! Everything runs in **one event loop**: local input, session output, model
//! snapshots, and timers. Anything that can block — here, connecting and
//! attaching to a session — runs off the UI thread (see [`session`]) and
//! reports back, because in one process a blocked call freezes every view.
//!
//! The sidebar is a renderer over shelbi-app's `SidebarModel` (built by a
//! background refresher); navigation state (selection, focus, sidebar width) is
//! shelbi-app's [`ClientState`]. This crate adds no model logic.

mod caps;
mod changes;
mod overlays;
mod refresh;
mod review;
mod session;
mod sidebar;
mod terminal_view;

#[cfg(all(test, unix))]
mod pty_input_tests;

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::cursor::Show;
use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
    EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::Terminal;

use shelbi_app::command::CommandRegistry;
use shelbi_app::exec::{Effect, Mutation};
use shelbi_app::nav::{ClientState, Focus, View};
use shelbi_app::view::SidebarModel;
use shelbi_app::CommandKind;
use shelbi_core::{ConfigMode, IssueTrackerConfig};
use shelbi_orchestrator::project_create::{self, ResolvedProjectRoot};
use shelbi_state::keymap::{load_keymaps, GlobalAction, KeyChord, Keymaps};
use shelbi_term::Size;

use crate::activity::ActivityApp;
use crate::kanban::{ExecutorMovePersister, KanbanApp};
use crate::machines::MachinesApp;
use caps::Caps;
use overlays::{ActiveOverlay, OverlayEvent};
use refresh::ShellSnapshot;
use review::{ReviewAction, ReviewInterface};
use session::{LiveConnector, MainState, SessionManager, SessionRef};
use sidebar::{RowTarget, SidebarView};

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

/// Resolved inputs the shell needs to build a [`ReviewInterface`], read off the
/// UI thread (board + project config lookups).
struct ReviewOpenParams {
    task_id: String,
    slot: String,
    worktree: String,
    editor_name: String,
    has_review_url: bool,
}

/// A message from a review background job (all run off the UI thread).
enum ReviewJobMsg {
    /// A review-open resolution finished (build the interface, or show the error).
    Opened(Result<ReviewOpenParams, String>),
    /// A daemon `Ensure` for a content role finished; bind the view on success.
    ContentReady(shelbi_app::ReviewRole, Result<(), String>),
    /// The gated merge (approve) finished.
    MergeDone(Result<(), String>),
    /// A reject-with-reason finished.
    RejectDone(Result<(), String>),
}

/// The result of the off-thread add-project job: the created project's slug (to
/// switch to on the UI thread), or a user-facing failure message.
enum CreateOutcome {
    Created(String),
    Failed(String),
}

/// The frame budget: redraws are capped at roughly 60 per second.
const FRAME: Duration = Duration::from_millis(16);
/// How often the background refresher is asked for fresh model data.
const REFRESH_INTERVAL: Duration = Duration::from_millis(750);
/// How long the one-time keyboard-protocol notice stays up.
const NOTICE_SECS: u64 = 6;

/// Run the single-process shell for `project` until the user quits.
pub fn run(project: &str) -> Result<()> {
    run_with(project, Arc::new(LiveConnector))
}

/// Run the shell against an injected connector (for tests that avoid a real
/// session). The terminal setup still happens, so this needs a TTY; the pure
/// routing is covered by unit tests instead.
fn run_with(project: &str, connector: Arc<dyn session::Connector>) -> Result<()> {
    // Detect capabilities before entering raw mode, while the terminal can
    // answer the kitty-protocol query.
    let caps = Caps::detect();

    let _guard = RawGuard::enter().context("entering raw mode")?;
    let mut term =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("initializing the terminal")?;

    let mut state = ShellState::new(project, connector, caps);

    // React to the daemon poller's layout events (review opened/closed/agent
    // recovered), with this process's own change bus as the setting-off
    // fallback — the same two sources the tmux sidebar drains
    // (`rt-daemon-layout-split`), now driving the native review interface.
    state.layout_bus = Some(shelbi_state::subscribe_changes());
    // Held for the loop's lifetime; dropping it stops the subscriber thread.
    let (_layout_sub, layout_rx) = crate::layout_sub::spawn(project);
    state.layout_rx = Some(layout_rx);

    let proj = project.to_string();
    let refresher = refresh::spawn(move |generation| read_snapshot(&proj, generation));
    refresher.request();
    let mut last_refresh = Instant::now();

    // Daemon change notifications drive refreshes (the native views don't poll);
    // the periodic request below is the fallback for a hubless project.
    let (_changes, change_rx) = changes::spawn(project);

    // Re-exec listener (Phase 4f): a background subscription to the daemon that
    // flips this flag when the daemon pushes a re-exec (this client is out of
    // date after an upgrade, or a `shelbi reload` signalled a re-exec). The loop
    // observes the flag and exits into the re-exec path below.
    let reexec = Arc::new(AtomicBool::new(false));
    spawn_reexec_listener(reexec.clone());

    // Start on the restored view (set by a prior re-exec) or the orchestrator
    // chat, focused.
    match reexec_restored_view() {
        Some(view) => state.apply_restored_view(view),
        None => state.show(RowTarget::Session(SessionRef::Orchestrator)),
    }

    let mut last_draw = Instant::now()
        .checked_sub(FRAME)
        .unwrap_or_else(Instant::now);

    loop {
        if state.should_quit {
            break;
        }
        // A pushed re-exec ends the loop and re-execs on the way out.
        if reexec.load(Ordering::SeqCst) && !state.should_reexec {
            state.should_reexec = true;
            state.should_quit = true;
            continue;
        }

        // Pace the loop on local input; this bounds the redraw rate too.
        if event::poll(FRAME).unwrap_or(false) {
            while event::poll(Duration::ZERO).unwrap_or(false) {
                match event::read() {
                    Ok(ev) => state.handle_event(ev),
                    Err(_) => break,
                }
                if state.should_quit {
                    break;
                }
            }
        }

        // Background sources (all non-blocking). A daemon change notification
        // triggers an immediate refresh; the periodic request is the fallback.
        let mut changed = false;
        while change_rx.try_recv().is_ok() {
            changed = true;
        }
        if changed || last_refresh.elapsed() >= REFRESH_INTERVAL {
            refresher.request();
            last_refresh = Instant::now();
        }
        if let Some(snap) = refresher.latest() {
            state.apply_snapshot(snap);
        }
        // Advance the board's background card-move persistence (settle a landed
        // hop, start the next queued one, or roll back on failure) so an optimistic
        // move resolves within a tick without ever blocking the loop — the same
        // call the standalone `__tasks` loop makes each tick. Keep redrawing while
        // a move is in flight so the settle / rollback shows.
        let had_moves = state.kanban.has_pending_moves();
        state.kanban.poll_pending_moves();
        if had_moves {
            state.dirty = true;
        }
        // Layout events (review opened/closed/agent recovered) from the daemon.
        if state.poll_layout_events() {
            state.dirty = true;
        }
        let mut ring = false;
        if state.sessions.pump_output(&mut ring) {
            state.dirty = true;
        }
        if state.sessions.poll() {
            state.dirty = true;
        }
        // The review interface's content terminal view runs its own connection.
        if let Some(r) = state.review.as_mut() {
            if r.pump_output(&mut ring) {
                state.dirty = true;
            }
            if r.poll() {
                state.dirty = true;
            }
            // Advance the merge spinner each tick while a gated merge runs.
            if r.is_merging() {
                r.tick_spinner();
                state.dirty = true;
            }
        }
        if state.poll_review() {
            state.dirty = true;
        }
        if state.poll_review_reconcile() {
            state.dirty = true;
        }
        if state.poll_job() {
            state.dirty = true;
        }
        if state.poll_create_job() {
            state.dirty = true;
        }
        if ring {
            let mut out = io::stdout();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }

        // Redraw, capped and wrapped in synchronized output.
        let now = Instant::now();
        if state.dirty && now.duration_since(last_draw) >= FRAME {
            draw(&mut term, &mut state)?;
            last_draw = now;
            state.dirty = false;
        }
    }

    // Re-exec on the way out, if the daemon asked us to. Restore the terminal
    // first (the RAII guard would do it on drop, but `exec` replaces the process
    // so Drop never runs), carry the current view forward so the fresh TUI lands
    // where this one was, then replace this process with the installed binary.
    if state.should_reexec {
        let view = state.client.view().clone();
        drop(term);
        drop(_guard); // restores the terminal
        reexec_into_current_binary(&view);
        // `reexec_into_current_binary` only returns if exec failed; fall through
        // to a clean exit so the shell doesn't hang in a broken terminal.
    }

    Ok(())
}

/// Env var carrying the view to restore across a re-exec, so an out-of-date TUI
/// that re-execs lands back on the view it was showing (per-client state).
const REEXEC_VIEW_ENV: &str = "SHELBI_REEXEC_VIEW";

/// The view a prior re-exec asked to restore, consumed once. `None` on a normal
/// start.
fn reexec_restored_view() -> Option<View> {
    let raw = std::env::var(REEXEC_VIEW_ENV).ok()?;
    std::env::remove_var(REEXEC_VIEW_ENV);
    if raw.is_empty() {
        return None;
    }
    Some(View::from_view_id(&raw))
}

/// Re-exec the installed `shelbi` with this process's own arguments, carrying
/// `view` forward in [`REEXEC_VIEW_ENV`]. Repeating the original argv restores
/// the project (it was on the command line); the env var restores the view. On
/// a non-Unix target, or if `exec` fails, this returns and the caller exits.
fn reexec_into_current_binary(view: &View) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    std::env::set_var(REEXEC_VIEW_ENV, view.as_view_id());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // `exec` only returns on failure.
        let _ = std::process::Command::new(exe).args(&args).exec();
    }
    #[cfg(not(unix))]
    {
        let _ = std::process::Command::new(exe).args(&args).status();
    }
}

/// Spawn the background re-exec listener: subscribe to the daemon's control
/// socket and flip `reexec` on a [`Notice::Reexec`] push. If the subscription
/// drops (the daemon restarted), re-probe the daemon version: a mismatch means
/// this client is now out of date, so flip the flag; otherwise reconnect. All
/// best-effort — a daemon that is simply absent leaves the flag clear, and the
/// shell keeps running (read-only session viewing is unaffected).
fn spawn_reexec_listener(reexec: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("shelbi-shell-reexec".into())
        .spawn(move || loop {
            if reexec.load(Ordering::SeqCst) {
                return;
            }
            match connect_control() {
                Some(client) => {
                    let Ok(mut sub) = client.subscribe() else {
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    };
                    loop {
                        match sub.recv() {
                            Ok(Some(shelbi_client::Notice::Reexec { .. })) => {
                                reexec.store(true, Ordering::SeqCst);
                                return;
                            }
                            Ok(Some(_)) => {} // a change note — ignored here
                            Ok(None) | Err(_) => break, // daemon closed / error
                        }
                    }
                    // The connection dropped. If the daemon came back on a new
                    // version, we are out of date → re-exec.
                    if matches!(
                        shelbi_state::daemon_version_status(),
                        shelbi_state::DaemonVersionStatus::Mismatch { .. }
                    ) {
                        reexec.store(true, Ordering::SeqCst);
                        return;
                    }
                }
                None => {
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        })
        .ok();
}

/// What the main area is currently showing.
#[derive(Clone)]
enum MainView {
    Session,
    Native(View),
    Review(String),
}

/// The three ways to leave the TUI (plan, "Quit semantics"):
///
/// - [`QuitAction::CloseUi`] — the default for `q`. Agents keep running; the
///   shell just stops rendering and drops its (detaching, not killing) session
///   connections, so reopening `shelbi` reattaches.
/// - [`QuitAction::QuitProject`] — end this project's sessions and mark it
///   closed, through the daemon's quit barrier.
/// - [`QuitAction::QuitShelbi`] — end all sessions and stop the daemon.
///
/// `CloseUi` is bound to `q`; `QuitProject`/`QuitShelbi` are invoked from the
/// command palette (`dispatch_effect`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuitAction {
    CloseUi,
    QuitProject,
    QuitShelbi,
}

/// The project/shelbi quit operations the shell drives through the daemon's
/// control socket, behind a trait so the shell's quit routing is testable
/// without a daemon (and so a `CloseUi` can be proven to touch neither).
trait ShellLifecycle: Send + Sync {
    fn quit_project(&self, project: &str);
    fn quit_shelbi(&self);
}

/// The production [`ShellLifecycle`]: connect to the daemon's control socket and
/// send the lifecycle command. Best-effort — a daemon that is already gone means
/// the sessions are already down, which is the desired end state anyway.
struct DaemonLifecycle;

impl ShellLifecycle for DaemonLifecycle {
    fn quit_project(&self, project: &str) {
        if let Some(mut client) = connect_control() {
            let _ = client.quit_project(project);
        }
    }
    fn quit_shelbi(&self) {
        if let Some(mut client) = connect_control() {
            let _ = client.quit_shelbi();
        }
    }
}

/// Connect to the daemon's control socket, or `None` if it isn't reachable.
fn connect_control() -> Option<shelbi_client::ControlClient> {
    let sock = shelbi_state::control_socket_path().ok()?;
    shelbi_client::ControlClient::connect(&sock, shelbi_state::CLIENT_VERSION).ok()
}

struct ShellState {
    client: ClientState,
    sessions: SessionManager,
    /// The connector, kept so a project switch can rebuild the session manager
    /// for the new project, and so a review interface can build its own content
    /// [`SessionManager`] (the main one is single-slot).
    connector: Arc<dyn session::Connector>,
    /// The project/shelbi quit operations (daemon control socket in production).
    lifecycle: Arc<dyn ShellLifecycle>,
    /// The open review interface (panel + content terminal view), when
    /// `main_view` is [`MainView::Review`]. `rt-tui-review`.
    review: Option<ReviewInterface>,
    /// In-flight review background work (open resolution, content ensure,
    /// approve, reject). Drained by [`ShellState::poll_review`].
    review_rx: Option<Receiver<ReviewJobMsg>>,
    /// Pushed layout events from the daemon poller over the hub socket
    /// (`rt-daemon-layout-split`), plus this process's own change bus as the
    /// setting-off fallback. Drained by [`ShellState::poll_layout_events`].
    layout_rx: Option<Receiver<shelbi_state::LayoutEvent>>,
    layout_bus: Option<shelbi_state::ChangeSubscription>,
    /// Throttle for the open-review close-reconcile (the board read runs off the
    /// UI thread, at most ~1 Hz, and only while a review is open).
    last_review_reconcile: Instant,
    /// In-flight close-reconcile read: delivers `(task, still_open)` so a review
    /// whose task has left the review column is dropped. Off-thread so a slow
    /// (remote) board read never blocks the UI.
    reconcile_rx: Option<Receiver<(String, bool)>>,
    caps: Caps,
    sidebar_model: Option<SidebarModel>,
    /// The embedded native views. They share their rendering (`render_full`) and
    /// interaction with the standalone tmux-runtime processes; the shell drives
    /// them from off-thread snapshots rather than their own refresh loop.
    kanban: KanbanApp,
    activity: ActivityApp,
    machines: MachinesApp,
    /// The user's keymaps (with this project's overrides), loaded once and
    /// shared by the embedded native-view key handlers, the palette's bindings,
    /// and resolving the palette-open chord.
    keymaps: Keymaps,
    main_view: MainView,
    /// Rects from the last draw, for mouse hit-testing.
    sidebar_rect: Rect,
    main_rect: Rect,
    /// The main-area size last reported to the live session.
    reported_main: Option<Size>,
    /// When `Some`, the user is typing a scrollback search query.
    search_input: Option<String>,
    /// Set when the shell should re-exec itself on exit (the daemon told it it
    /// is out of date, or a `shelbi reload` signalled a re-exec). Checked by
    /// [`run_with`] after the event loop ends.
    should_reexec: bool,
    /// The one-time keyboard-protocol notice: shown from startup until its
    /// deadline, then cleared and never re-armed (see [`ShellState::notice_text`]).
    notice: Option<Notice>,
    /// The open in-process overlay (removing-tmux Phase 4d), if any. Only one is
    /// open at a time; while open it captures input and draws over the main area.
    overlay: Option<ActiveOverlay>,
    /// The configured palette-open chord (`GlobalAction::OpenPalette`). The
    /// palette also always opens on Ctrl+Space (the plan's reserved key), so the
    /// opener is the union of this chord and Ctrl+Space.
    palette_chord: Option<KeyChord>,
    /// A transient status line shown at the bottom of the main area (effect
    /// results, deferred-feature notes, background-job outcomes).
    status: Option<String>,
    /// A pending off-UI-thread job; its `String` is the status to show when it
    /// finishes. Blocking commands (e.g. the Zen toggle) run here so they never
    /// freeze the one event loop.
    job: Option<Receiver<String>>,
    /// A pending off-UI-thread add-project scaffold. Separate from [`Self::job`]
    /// because on success the shell must switch to the new project on the UI
    /// thread, so the result is typed ([`CreateOutcome`]) rather than a bare
    /// status string.
    create_job: Option<Receiver<CreateOutcome>>,
    should_quit: bool,
    dirty: bool,
}

/// The startup keyboard-protocol notice and the instant it auto-hides at.
struct Notice {
    text: &'static str,
    until: Instant,
}

impl ShellState {
    fn new(project: &str, connector: Arc<dyn session::Connector>, caps: Caps) -> Self {
        Self::new_with(project, connector, caps, Arc::new(DaemonLifecycle))
    }

    fn new_with(
        project: &str,
        connector: Arc<dyn session::Connector>,
        caps: Caps,
        lifecycle: Arc<dyn ShellLifecycle>,
    ) -> Self {
        let notice = caps.keyboard_notice().map(|text| Notice {
            text,
            until: Instant::now() + Duration::from_secs(NOTICE_SECS),
        });
        // Load the keymaps once: the palette's bindings and opener chord come
        // from here, and the embedded native-view handlers share them. A load
        // failure degrades to the embedded defaults; the sidebar surfaces the
        // diagnostic count.
        let (keymaps, _diags) = load_keymaps(Some(project));
        let palette_chord = keymaps
            .global
            .first_chord_for(GlobalAction::OpenPalette)
            .copied();
        let mut kanban = KanbanApp::new(project);
        kanban.keymaps = keymaps.clone();
        // Board moves go through the shelbi-app executor (daemon-backed when
        // `dev.daemon_mutations` is on); the standalone process keeps its direct path.
        kanban.move_persister = Some(Arc::new(ExecutorMovePersister));
        // The embedded machines view probes remote reachability in the
        // background (off the UI thread and the refresh worker); the prober
        // winds down when the shell state drops.
        let mut machines = MachinesApp::new(project);
        machines.enable_reachability();
        Self {
            client: ClientState::new(project),
            sessions: SessionManager::new(project, connector.clone()),
            connector,
            lifecycle,
            review: None,
            review_rx: None,
            layout_rx: None,
            layout_bus: None,
            last_review_reconcile: Instant::now(),
            reconcile_rx: None,
            caps,
            sidebar_model: None,
            kanban,
            activity: ActivityApp::new(project),
            machines,
            keymaps,
            main_view: MainView::Session,
            sidebar_rect: Rect::default(),
            main_rect: Rect::default(),
            reported_main: None,
            search_input: None,
            should_reexec: false,
            notice,
            overlay: None,
            palette_chord,
            status: None,
            job: None,
            create_job: None,
            should_quit: false,
            dirty: true,
        }
    }

    /// The keyboard-protocol notice text to paint at `now`, or `None`. The
    /// notice shows from startup until its deadline; once the deadline passes it
    /// is cleared and this returns `None` forever after, so the banner is
    /// emitted exactly once per run and never re-armed.
    fn notice_text(&mut self, now: Instant) -> Option<&'static str> {
        match &self.notice {
            Some(n) if now < n.until => Some(n.text),
            Some(_) => {
                self.notice = None;
                None
            }
            None => None,
        }
    }

    fn apply_snapshot(&mut self, snap: ShellSnapshot) {
        if let Some(sidebar) = snap.sidebar {
            // Keep the selection in range as rows come and go.
            let view = SidebarView::build(&sidebar);
            self.client.clamp_selection(view.selectable_count());
            self.sidebar_model = Some(sidebar);
            // Keep an open palette's command list warm as the board refreshes.
            // Compute the entries first so the `&mut self.overlay` borrow doesn't
            // overlap the `&self` read in `palette_entries_from_model`.
            if self.overlay.is_some() {
                let entries = self.palette_entries_from_model();
                if let Some(ov) = &mut self.overlay {
                    ov.refresh_palette(entries);
                }
            }
            self.dirty = true;
        }
        // Fold the off-thread reads into the embedded native views (cheap, never
        // blocks). The apps keep their own interaction state (selection, scroll,
        // dropdowns, in-flight optimistic moves); this only swaps the data.
        if let Some(board) = snap.board {
            self.kanban.apply_board_data(board);
            self.dirty = true;
        }
        if let Some(activity) = snap.activity {
            self.activity.apply_activity_data(activity);
            self.dirty = true;
        }
        if let Some(machines) = snap.machines {
            self.machines.apply_data(machines);
            self.dirty = true;
        }
    }

    /// Build the palette's entries from the current sidebar model via the
    /// shelbi-app command registry. Empty before the first snapshot lands.
    fn palette_entries_from_model(&self) -> Vec<shelbi_palette::Entry> {
        let (Some(project), Some(sidebar)) = (self.client.project(), self.sidebar_model.as_ref())
        else {
            return Vec::new();
        };
        let model = overlays::build_command_model(project, sidebar);
        CommandRegistry::new().entries(&model)
    }

    fn sidebar_view(&self) -> Option<SidebarView> {
        self.sidebar_model.as_ref().map(SidebarView::build)
    }

    fn selection(&self) -> usize {
        self.client.sidebar_selection()
    }

    fn sidebar_width(&self) -> u16 {
        self.client.sidebar_width()
    }

    /// Open a sidebar target: a session shows in the main area and takes focus;
    /// a native view just swaps the main area (focus stays on the sidebar until
    /// the user steps in with Ctrl+Space). Records the target as the current
    /// project's remembered view (via [`ClientState::set_view`]) so a later
    /// [`switch_project`](Self::switch_project) back to this project restores it
    /// — the per-client last-view-per-project memory the plan wants.
    fn show(&mut self, target: RowTarget) {
        // Record the view first (a Review target has no `View` and is transient,
        // so it is deliberately not remembered).
        if let Some(view) = row_target_to_view(&target) {
            self.client.set_view(view);
        }
        match target {
            RowTarget::Session(r) => {
                self.main_view = MainView::Session;
                self.reported_main = None; // force a resize report for the new session
                // Record the view (orchestrator chat or a workspace session).
                let view = match &r {
                    SessionRef::Orchestrator => View::Session("orch".into()),
                    SessionRef::Workspace(w) => View::Session(w.clone()),
                    // Review content sessions live inside the review interface's
                    // own manager, never the main one; this arm only keeps the
                    // match exhaustive.
                    SessionRef::Review { slot, role } => {
                        View::Session(format!("review/{slot}/{role}"))
                    }
                };
                self.client.set_view(view);
                self.sessions.show(r);
                self.client.focus_main();
            }
            RowTarget::Native(v) => {
                self.client.set_view(v.clone());
                self.main_view = MainView::Native(v);
            }
            RowTarget::Review(id) => {
                // Review has no `View` variant (it is a transient interface, not a
                // remembered main view), so it is not recorded.
                self.open_review(id);
            }
        }
        self.dirty = true;
    }

    /// Switch this client to another open project, restoring the view it last
    /// had there (its default view the first time). The session manager is
    /// rebuilt for the new project and the restored view is applied to the main
    /// area. Agents in the old project keep running — switching is a view
    /// change, not a quit. Invoked from the command palette (`dispatch_effect`).
    fn switch_project(&mut self, project: &str) {
        // `ClientState::switch_project` records the current project's view and
        // restores the new project's remembered (or default) view.
        self.client.switch_project(project);
        // Rebuild the session manager for the new project (dropping the old
        // project's connection detaches, never kills).
        self.sessions = SessionManager::new(project, self.connector.clone());
        self.reported_main = None;
        // Apply the restored view to the main area.
        let restored = self.client.view().clone();
        self.apply_restored_view(restored);
        self.dirty = true;
    }

    /// Apply a [`View`] (as restored by a project switch) to the main area
    /// without re-recording it (it is already the client's current view).
    fn apply_restored_view(&mut self, view: View) {
        match view_to_row_target(&view) {
            RowTarget::Session(r) => {
                self.main_view = MainView::Session;
                self.sessions.show(r);
                // A restored session view keeps sidebar focus — a switch lands
                // the user on the sidebar (see `ClientState::switch_project`).
            }
            RowTarget::Native(v) => self.main_view = MainView::Native(v),
            RowTarget::Review(id) => self.main_view = MainView::Review(id),
        }
    }

    /// Leave the TUI via `action`. `CloseUi` touches no sessions (they outlive
    /// the client); `QuitProject`/`QuitShelbi` go through the daemon's control
    /// socket. All three stop the event loop.
    fn quit(&mut self, action: QuitAction) {
        match action {
            QuitAction::CloseUi => {
                // Nothing to do to the sessions: dropping the shell detaches
                // (never kills) its connections, so agents keep running and a
                // reopen reattaches.
            }
            QuitAction::QuitProject => {
                if let Some(project) = self.client.project() {
                    self.lifecycle.quit_project(project);
                }
            }
            QuitAction::QuitShelbi => {
                self.lifecycle.quit_shelbi();
            }
        }
        self.should_quit = true;
    }

    fn activate_selection(&mut self) {
        if let Some(view) = self.sidebar_view() {
            if let Some(target) = view.target_at(self.selection()) {
                self.show(target);
            }
        }
    }

    // --- event handling ----------------------------------------------------

    fn handle_event(&mut self, ev: Event) {
        // An open overlay captures input: keys and clicks drive it, not the
        // sidebar or the agent beneath it.
        if self.overlay.is_some() {
            match ev {
                Event::Key(k) if terminal_view::is_actionable(&k) => {
                    let outcome = self
                        .overlay
                        .as_mut()
                        .unwrap()
                        .handle_key(k, &self.keymaps);
                    self.apply_overlay_event(outcome);
                    self.dirty = true;
                }
                Event::Mouse(m) => {
                    let area = self.main_rect;
                    let outcome = self.overlay.as_mut().unwrap().handle_mouse(m, area);
                    self.apply_overlay_event(outcome);
                    self.dirty = true;
                }
                Event::Resize(_, _) => self.dirty = true,
                // Bracketed paste, focus changes, and key releases are ignored
                // while an overlay is up.
                _ => {}
            }
            return;
        }
        match ev {
            Event::Key(k) => self.handle_key(k),
            Event::Mouse(m) => self.handle_mouse(m),
            Event::Paste(s) => {
                if self.focus_is_main() {
                    self.sessions.send_paste(&s);
                    self.dirty = true;
                }
            }
            Event::FocusGained => self.forward_focus(true),
            Event::FocusLost => self.forward_focus(false),
            Event::Resize(_, _) => self.dirty = true,
        }
    }

    fn focus_is_main(&self) -> bool {
        self.client.focus() == Focus::Main
    }

    fn handle_key(&mut self, k: KeyEvent) {
        if !terminal_view::is_actionable(&k) {
            return;
        }
        // The palette-open chord opens the command palette from anywhere — a
        // focused terminal view or the sidebar. This replaces the interim
        // Ctrl+Space focus-toggle from rt-tui-shell.
        if self.is_palette_open(&k) {
            self.open_palette();
            self.dirty = true;
            return;
        }
        if self.focus_is_main() {
            self.handle_main_key(k);
        } else {
            self.handle_sidebar_key(k);
        }
    }

    /// Whether `k` opens the palette: the configured `OpenPalette` chord, or
    /// Ctrl+Space (the plan's permanent reserved key; also delivered as Ctrl+@
    /// by many terminals).
    fn is_palette_open(&self, k: &KeyEvent) -> bool {
        if terminal_view::is_focus_key(k) {
            return true;
        }
        match self.palette_chord {
            Some(chord) => crate::keymap::chord_from_event(*k) == Some(chord),
            None => false,
        }
    }

    /// Open the command palette over the current registry entries.
    fn open_palette(&mut self) {
        let label = self
            .sidebar_model
            .as_ref()
            .map(|s| s.project_label.clone())
            .or_else(|| self.client.project().map(str::to_string))
            .unwrap_or_default();
        let entries = self.palette_entries_from_model();
        self.overlay = Some(ActiveOverlay::palette(&label, entries));
    }

    /// Act on what the active overlay yielded.
    fn apply_overlay_event(&mut self, event: OverlayEvent) {
        match event {
            OverlayEvent::Stay => {}
            OverlayEvent::Close => {
                self.overlay = None;
                self.client.focus_main();
            }
            OverlayEvent::FocusSidebar => {
                self.overlay = None;
                self.client.focus_sidebar();
            }
            OverlayEvent::RunEntry(entry) => {
                // Close the palette first; the command's effect may open a
                // different overlay (error log / Zen intro) in its place.
                self.overlay = None;
                self.client.focus_main();
                self.run_entry(&entry);
            }
            OverlayEvent::ReviewConfirmed { task_id, slot } => {
                self.overlay = None;
                self.client.focus_main();
                // The confirm dialog returns the chosen slot as a value; wiring
                // it to the load-for-review transition is the review interface
                // (rt-tui-review, Phase 4e).
                self.status = Some(format!(
                    "Load {task_id} onto {slot} — review loading lands in the review interface"
                ));
            }
            OverlayEvent::ReviewCancelled => {
                self.overlay = None;
                self.client.focus_main();
            }
            OverlayEvent::Rejected { task_id, reason } => {
                self.overlay = None;
                self.client.focus_main();
                self.submit_reject(task_id, reason);
            }
            OverlayEvent::ZenResult {
                project,
                confirmed,
                dont_show_again,
            } => {
                self.overlay = None;
                self.client.focus_main();
                self.apply_zen_intro(project, confirmed, dont_show_again);
            }
            OverlayEvent::AddProjectCancel => {
                self.overlay = None;
                self.client.focus_main();
            }
            OverlayEvent::AddProjectSubmit { name, root, mode } => {
                self.submit_add_project(name, root, mode)
            }
        }
    }

    /// Validate the submitted add-project form. On success, close the form and
    /// scaffold + switch off the UI thread; on failure, keep the form open with
    /// the inline error so the user can fix the input. Validation itself is
    /// cheap (a slug check, a collision lookup, a path stat) so it runs here.
    fn submit_add_project(&mut self, name: String, root: String, mode: ConfigMode) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        match project_create::validate_add_project(&name, &root, &cwd) {
            Ok(resolved) => {
                self.overlay = None;
                self.client.focus_main();
                self.create_project(resolved, mode);
            }
            Err(msg) => {
                if let Some(ActiveOverlay::AddProject { form }) = self.overlay.as_mut() {
                    form.set_error(msg);
                }
                self.dirty = true;
            }
        }
    }

    /// Scaffold the validated project off the UI thread (file IO can block),
    /// pinning the `file_system` board the form has no control over. On success
    /// [`Self::poll_create_job`] switches to it on the UI thread.
    fn create_project(&mut self, resolved: ResolvedProjectRoot, mode: ConfigMode) {
        let (tx, rx) = std::sync::mpsc::channel();
        let slug = resolved.name.clone();
        if std::thread::Builder::new()
            .name("shelbi-add-project".into())
            .spawn(move || {
                let outcome = match project_create::scaffold_project(
                    &resolved,
                    mode,
                    &IssueTrackerConfig::default(),
                    &mut project_create::NullReporter,
                ) {
                    Ok(()) => CreateOutcome::Created(slug),
                    Err(e) => CreateOutcome::Failed(format!("Add project failed: {e}")),
                };
                let _ = tx.send(outcome);
            })
            .is_ok()
        {
            self.status = Some("Creating project…".to_string());
            self.create_job = Some(rx);
        }
    }

    /// Poll the pending add-project scaffold without blocking. On success,
    /// switch to the freshly created project; on failure, surface the error.
    /// Returns `true` when the job finished (so the caller redraws).
    fn poll_create_job(&mut self) -> bool {
        let outcome = match &self.create_job {
            Some(rx) => match rx.try_recv() {
                Ok(o) => o,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => {
                    CreateOutcome::Failed("add project stopped unexpectedly".to_string())
                }
            },
            None => return false,
        };
        self.create_job = None;
        match outcome {
            CreateOutcome::Created(slug) => {
                self.switch_project(&slug);
                self.status = Some(format!("Created project {slug} — switched to it"));
            }
            CreateOutcome::Failed(msg) => self.status = Some(msg),
        }
        true
    }

    /// Resolve a palette entry to its command effect and dispatch it.
    fn run_entry(&mut self, entry: &shelbi_palette::Entry) {
        let Some(project) = self.client.project().map(str::to_string) else {
            return;
        };
        match CommandKind::from_id(&entry.id) {
            Some(kind) => {
                let effect = kind.effect_with_project(&project);
                self.dispatch_effect(effect);
            }
            None => self.status = Some(format!("unknown command: {}", entry.id)),
        }
    }

    /// Carry out a command [`Effect`]. Navigation effects change the main view
    /// directly; the error log and Zen intro open as overlays; the Zen toggle
    /// runs off the UI thread. Effects whose home is a later Phase 4 subtask
    /// (switch/add/quit project, opening an external editor, loading a review)
    /// surface a status note rather than silently doing nothing.
    fn dispatch_effect(&mut self, effect: Effect) {
        match effect {
            Effect::ShowView(v) => self.show_view_effect(v),
            Effect::FocusWorkspace { workspace, .. } => {
                self.show(RowTarget::Session(SessionRef::Workspace(workspace)))
            }
            Effect::LoadReview { task_id, .. } => self.open_review(task_id),
            Effect::FocusSession { session } => {
                self.status = Some(format!(
                    "focus session {session} — legacy spawned agents aren't shown here yet"
                ))
            }
            Effect::OpenErrorLog { project } => {
                self.overlay = Some(ActiveOverlay::error_log(project));
            }
            Effect::OpenEditor { target } => {
                self.status = Some(format!(
                    "edit {target:?} — opening an external editor lands in a later Phase 4 subtask"
                ))
            }
            Effect::SwitchProject { project } => self.switch_project(&project),
            Effect::AddProject => {
                // Open the in-process add-project form over the main area. On
                // submit the shell validates + scaffolds through the shared
                // `project_create` engine (the same path `shelbi init` uses),
                // off the UI thread, then switches to the new project.
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                self.overlay = Some(ActiveOverlay::add_project(&cwd));
            }
            Effect::QuitProject { .. } => self.quit(QuitAction::QuitProject),
            Effect::QuitShelbi => self.quit(QuitAction::QuitShelbi),
            Effect::Mutate(Mutation::ToggleZen { project }) => self.toggle_zen(project),
            Effect::Mutate(other) => {
                self.status = Some(format!("mutation {other:?} is not reachable from the palette"))
            }
        }
    }

    /// Navigate to a view. Native views swap the main area; the orchestrator
    /// "chat" and workspace session views bind a terminal view.
    fn show_view_effect(&mut self, v: View) {
        match v {
            View::Issues | View::Activity | View::Machines => self.show(RowTarget::Native(v)),
            View::Session(name) if name == "orch" => {
                self.show(RowTarget::Session(SessionRef::Orchestrator))
            }
            View::Session(name) => self.show(RowTarget::Session(SessionRef::Workspace(name))),
        }
    }

    /// Toggle Zen Mode. On a first off→on transition the intro overlay shows
    /// first (and performs the toggle on confirm); otherwise the toggle runs off
    /// the UI thread.
    fn toggle_zen(&mut self, project: String) {
        if overlays::should_show_zen_intro(&project) {
            self.overlay = Some(ActiveOverlay::zen_intro(project));
            return;
        }
        self.spawn_job(move || match toggle_zen_blocking(&project) {
            Ok(msg) => msg,
            Err(e) => format!("Zen toggle failed: {e}"),
        });
    }

    /// Apply the Zen-intro result off the UI thread: persist the "don't show
    /// again" flag, and toggle Zen on confirm. Mirrors the tmux palette's
    /// `apply_zen_intro_result`.
    fn apply_zen_intro(&mut self, project: String, confirmed: bool, dont_show_again: bool) {
        self.spawn_job(move || {
            if dont_show_again {
                let _ = shelbi_state::mark_zen_intro_seen();
            }
            if !confirmed {
                return "Zen Mode: cancelled".to_string();
            }
            match toggle_zen_blocking(&project) {
                Ok(msg) => msg,
                Err(e) => format!("Zen toggle failed: {e}"),
            }
        });
    }

    /// Spawn `f` on a worker thread; its returned string becomes the status line
    /// when it finishes. Blocking commands run here so they never freeze the one
    /// event loop (plan: "commands that can block run off the UI thread").
    fn spawn_job(&mut self, f: impl FnOnce() -> String + Send + 'static) {
        let (tx, rx) = std::sync::mpsc::channel();
        if std::thread::Builder::new()
            .name("shelbi-shell-job".into())
            .spawn(move || {
                let _ = tx.send(f());
            })
            .is_ok()
        {
            self.status = Some("working…".to_string());
            self.job = Some(rx);
        }
    }

    /// Poll a pending off-thread job without blocking. Returns `true` when it
    /// finished (so the caller redraws the status line).
    fn poll_job(&mut self) -> bool {
        let done = match &self.job {
            Some(rx) => match rx.try_recv() {
                Ok(msg) => msg,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => "job stopped unexpectedly".to_string(),
            },
            None => return false,
        };
        self.job = None;
        self.status = Some(done);
        true
    }

    // --- review interface (rt-tui-review) ----------------------------------

    /// Begin opening the review interface for `task_id`: resolve its slot,
    /// worktree, editor, and review-URL off the UI thread, then build the
    /// interface when that lands (see [`ShellState::poll_review`]). The main
    /// area switches to the Review view immediately so the user sees progress.
    fn open_review(&mut self, task_id: String) {
        // Already showing this review — no-op.
        if self.review.as_ref().map(|r| r.task_id()) == Some(task_id.as_str())
            && matches!(self.main_view, MainView::Review(_))
        {
            return;
        }
        self.main_view = MainView::Review(task_id.clone());
        self.client.focus_main();
        let project = self
            .client
            .project()
            .map(str::to_string)
            .unwrap_or_default();
        let (tx, rx) = std::sync::mpsc::channel();
        self.review_rx = Some(rx);
        self.status = Some(format!("opening review {task_id}…"));
        std::thread::Builder::new()
            .name("shelbi-review-open".into())
            .spawn(move || {
                let _ = tx.send(ReviewJobMsg::Opened(resolve_review_open(&project, &task_id)));
            })
            .ok();
    }

    /// Drain a finished review background job without blocking. Returns `true`
    /// when something changed.
    fn poll_review(&mut self) -> bool {
        let msg = match &self.review_rx {
            Some(rx) => match rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => {
                    self.review_rx = None;
                    return false;
                }
            },
            None => return false,
        };
        self.review_rx = None;
        match msg {
            ReviewJobMsg::Opened(Ok(p)) => {
                // Only build if we're still meant to be on this review.
                if matches!(&self.main_view, MainView::Review(id) if *id == p.task_id) {
                    let project = self
                        .client
                        .project()
                        .map(str::to_string)
                        .unwrap_or_default();
                    self.review = Some(ReviewInterface::new(
                        &project,
                        self.connector.clone(),
                        p.task_id,
                        p.slot,
                        p.worktree,
                        p.editor_name,
                        p.has_review_url,
                    ));
                    self.reported_main = None; // re-report size for the content view
                    self.status = None;
                }
            }
            ReviewJobMsg::Opened(Err(e)) => {
                self.status = Some(format!("review failed: {e}"));
                // Drop back to the orchestrator chat.
                self.show(RowTarget::Session(SessionRef::Orchestrator));
            }
            ReviewJobMsg::ContentReady(role, Ok(())) => {
                if let Some(r) = self.review.as_mut() {
                    r.show_role(role);
                    self.reported_main = None;
                }
            }
            ReviewJobMsg::ContentReady(_, Err(e)) => {
                if let Some(r) = self.review.as_mut() {
                    r.set_status(format!("open view failed: {e}"));
                    r.show_chat();
                }
            }
            ReviewJobMsg::MergeDone(Ok(())) => self.finish_review_close("approved"),
            ReviewJobMsg::MergeDone(Err(e)) => {
                if let Some(r) = self.review.as_mut() {
                    r.set_merging(false);
                    r.set_status(format!("approve failed: {e}"));
                }
            }
            ReviewJobMsg::RejectDone(Ok(())) => self.finish_review_close("rejected"),
            ReviewJobMsg::RejectDone(Err(e)) => {
                if let Some(r) = self.review.as_mut() {
                    r.set_merging(false);
                    r.set_status(format!("reject failed: {e}"));
                }
            }
        }
        true
    }

    /// Carry out one [`ReviewAction`] from the embedded panel. Blocking parts
    /// run off the UI thread and report back through [`ReviewJobMsg`].
    fn apply_review_action(&mut self, action: ReviewAction) {
        let (Some(project), Some(review)) = (
            self.client.project().map(str::to_string),
            self.review.as_ref(),
        ) else {
            return;
        };
        let task = review.task_id().to_string();
        match action {
            ReviewAction::None => {}
            ReviewAction::ShowContent(None) => {
                if let Some(r) = self.review.as_mut() {
                    r.show_chat();
                    self.reported_main = None;
                }
            }
            ReviewAction::ShowContent(Some(role)) => {
                // Ask the daemon to ensure the editor/diff session, then bind it.
                let (tx, rx) = std::sync::mpsc::channel();
                self.review_rx = Some(rx);
                std::thread::Builder::new()
                    .name("shelbi-review-ensure".into())
                    .spawn(move || {
                        let op = shelbi_app::ReviewSessionOp::Ensure { role };
                        let res = shelbi_app::review_session(&project, &task, op, &mut |_, _| {})
                            .map_err(|e| e.to_string());
                        let _ = tx.send(ReviewJobMsg::ContentReady(role, res));
                    })
                    .ok();
            }
            ReviewAction::Approve => {
                if let Some(r) = self.review.as_mut() {
                    r.set_merging(true);
                }
                let (tx, rx) = std::sync::mpsc::channel();
                self.review_rx = Some(rx);
                std::thread::Builder::new()
                    .name("shelbi-review-approve".into())
                    .spawn(move || {
                        let m = shelbi_app::Mutation::ApproveReview {
                            project: project.clone(),
                            id: task.clone(),
                        };
                        let res = run_review_mutation(&m);
                        let _ = tx.send(ReviewJobMsg::MergeDone(res));
                    })
                    .ok();
            }
            ReviewAction::Reject => {
                self.overlay = Some(ActiveOverlay::reject_reason(task));
            }
            ReviewAction::OpenBrowser | ReviewAction::RevealFolder => {
                // Openers run inline (spawn+detach); failures land on the status.
                if let Some(r) = self.review.as_mut() {
                    r.set_status("opening…");
                }
                self.spawn_review_opener(action);
            }
            ReviewAction::Back => {
                // Leave the interface loaded (daemon sessions stay); just return
                // to the orchestrator chat.
                self.review = None;
                self.show(RowTarget::Session(SessionRef::Orchestrator));
            }
            ReviewAction::Close => self.close_review(&project, &task),
        }
    }

    /// Open the browser / reveal the folder for the current review, off-thread.
    fn spawn_review_opener(&mut self, action: ReviewAction) {
        let (Some(project), Some(review)) = (
            self.client.project().map(str::to_string),
            self.review.as_ref(),
        ) else {
            return;
        };
        let task = review.task_id().to_string();
        self.spawn_job(move || match action {
            ReviewAction::OpenBrowser => match review::open_browser(&project, &task) {
                Ok(()) => "opened browser".to_string(),
                Err(e) => format!("open browser failed: {e}"),
            },
            ReviewAction::RevealFolder => match review::reveal_folder(&project, &task) {
                Ok(()) => "revealed folder".to_string(),
                Err(e) => format!("reveal failed: {e}"),
            },
            _ => String::new(),
        });
    }

    /// Close the review: ask the daemon to end the editor/diff/server sessions
    /// (freeing the port), drop the interface, and return to the orchestrator.
    fn close_review(&mut self, project: &str, task: &str) {
        let project = project.to_string();
        let task = task.to_string();
        self.spawn_job(move || {
            let op = shelbi_app::ReviewSessionOp::Close;
            match shelbi_app::review_session(&project, &task, op, &mut |_, _| {}) {
                Ok(()) => "review closed".to_string(),
                Err(e) => format!("review close failed: {e}"),
            }
        });
        self.review = None;
        self.show(RowTarget::Session(SessionRef::Orchestrator));
    }

    /// Finish an approve/reject: the task has left review, so tear the interface
    /// down (the daemon frees the sessions and port) and return to the chat.
    fn finish_review_close(&mut self, how: &str) {
        let project = self
            .client
            .project()
            .map(str::to_string)
            .unwrap_or_default();
        if let Some(r) = self.review.as_ref() {
            let task = r.task_id().to_string();
            let p = project.clone();
            self.spawn_job(move || {
                let op = shelbi_app::ReviewSessionOp::Close;
                let _ = shelbi_app::review_session(&p, &task, op, &mut |_, _| {});
                String::new()
            });
        }
        self.review = None;
        self.status = Some(format!("review {how}"));
        self.show(RowTarget::Session(SessionRef::Orchestrator));
    }

    /// Submit a reject-with-reason (from the reject overlay) off the UI thread.
    /// Like approve, the review is held inert while it runs.
    fn submit_reject(&mut self, task_id: String, reason: String) {
        let Some(project) = self.client.project().map(str::to_string) else {
            return;
        };
        if let Some(r) = self.review.as_mut() {
            r.set_merging(true);
        } else {
            self.status = Some(format!("rejecting {task_id}…"));
        }
        let (tx, rx) = std::sync::mpsc::channel();
        self.review_rx = Some(rx);
        std::thread::Builder::new()
            .name("shelbi-review-reject".into())
            .spawn(move || {
                let m = shelbi_app::Mutation::RejectReview {
                    project,
                    id: task_id,
                    reason,
                };
                let res = run_review_mutation(&m);
                let _ = tx.send(ReviewJobMsg::RejectDone(res));
            })
            .ok();
    }

    // --- layout events (rt-daemon-layout-split, driven natively) -----------

    /// Drain pushed layout events and the in-process change bus, applying each.
    /// Also reconciles the open review against durable state so a `ReviewClosed`
    /// missed while detached (or any stale review view) is dropped — the
    /// close-reconcile the tmux path never needed (`rt-tui-review`).
    fn poll_layout_events(&mut self) -> bool {
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = self.layout_rx.as_ref() {
            loop {
                match rx.try_recv() {
                    Ok(ev) => events.push(ev),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        if disconnected {
            self.layout_rx = None;
        }
        let project = self.client.project().map(str::to_string);
        if let (Some(bus), Some(project)) = (self.layout_bus.as_ref(), project.as_ref()) {
            while let Some(change) = bus.try_recv() {
                if change.project() == *project {
                    if let Some(ev) = change.layout() {
                        events.push(ev.clone());
                    }
                }
            }
        }
        let had = !events.is_empty();
        for ev in events {
            self.apply_layout_event(ev);
        }
        // Kick off a close-reconcile so a review whose task has left the review
        // column is dropped even if its ReviewClosed event was missed while this
        // client was detached. Throttled (after any real layout event, else at
        // most ~1 Hz) and run off the UI thread, so a slow (remote) board read
        // never blocks the loop. Only while a review is actually open.
        if (had || self.last_review_reconcile.elapsed() >= Duration::from_secs(1))
            && self.reconcile_rx.is_none()
        {
            self.spawn_review_reconcile();
        }
        had
    }

    /// Start an off-thread board read that reports whether the open review's
    /// task is still in the review column (the close-reconcile source).
    fn spawn_review_reconcile(&mut self) {
        let MainView::Review(task) = &self.main_view else {
            return;
        };
        let task = task.clone();
        let Some(project) = self.client.project().map(str::to_string) else {
            return;
        };
        self.last_review_reconcile = Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        if std::thread::Builder::new()
            .name("shelbi-review-reconcile".into())
            .spawn(move || {
                let still = review_task_still_open(&project, &task);
                let _ = tx.send((task, still));
            })
            .is_ok()
        {
            self.reconcile_rx = Some(rx);
        }
    }

    /// Apply a finished close-reconcile read. Returns `true` if it dropped a
    /// stale review view.
    fn poll_review_reconcile(&mut self) -> bool {
        let (task, still_open) = match &self.reconcile_rx {
            Some(rx) => match rx.try_recv() {
                Ok(v) => v,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => {
                    self.reconcile_rx = None;
                    return false;
                }
            },
            None => return false,
        };
        self.reconcile_rx = None;
        if !still_open && self.showing_review(&task) {
            self.review = None;
            self.status = Some(format!("review {task} closed"));
            self.show(RowTarget::Session(SessionRef::Orchestrator));
            return true;
        }
        false
    }

    fn apply_layout_event(&mut self, ev: shelbi_state::LayoutEvent) {
        use shelbi_state::LayoutEvent::*;
        match ev {
            ReviewClosed { task, .. } => {
                // If we're showing that review, drop it and return to chat. The
                // daemon already freed the slot; no client teardown needed.
                if self.showing_review(&task) {
                    self.review = None;
                    self.status = Some(format!("review {task} closed"));
                    self.show(RowTarget::Session(SessionRef::Orchestrator));
                }
            }
            // Opened/AgentRecovered: the sessions are the daemon's half; the
            // interface opens on user selection and the content view reconnects
            // on its own. A refresh keeps the sidebar current. OrchestratorRestarted
            // likewise needs nothing here (the main session reconnects).
            ReviewOpened { .. } | ReviewAgentRecovered { .. } | OrchestratorRestarted => {}
        }
    }

    /// Whether the main area is currently showing the review for `task`.
    fn showing_review(&self, task: &str) -> bool {
        matches!(&self.main_view, MainView::Review(id) if id == task)
            && self.review.as_ref().map(|r| r.task_id()) == Some(task)
    }

    fn handle_sidebar_key(&mut self, k: KeyEvent) {
        let count = self
            .sidebar_view()
            .map(|v| v.selectable_count())
            .unwrap_or(0);
        match k.code {
            KeyCode::Up => self.client.select_up(),
            KeyCode::Down | KeyCode::Tab => self.client.select_down(count),
            KeyCode::BackTab => self.client.select_up(),
            KeyCode::Enter => self.activate_selection(),
            // `q` closes the UI (the default quit): agents keep running and a
            // reopen reattaches. Quit-project / quit-Shelbi are the palette's
            // (Phase 4d) job, routed through [`ShellState::quit`].
            KeyCode::Char('q') => self.quit(QuitAction::CloseUi),
            _ => {}
        }
        self.dirty = true;
    }

    fn handle_main_key(&mut self, k: KeyEvent) {
        // The review interface owns the main area when it is open: keys drive
        // the panel or forward to its content view (rt-tui-review).
        if matches!(self.main_view, MainView::Review(_)) {
            if let Some(r) = self.review.as_mut() {
                let action = r.handle_key(k);
                self.apply_review_action(action);
                self.dirty = true;
            }
            return;
        }
        // A native view handles its own keys (board nav / moves / popover /
        // dropdowns, activity scroll / filters, machines nav / open). The session
        // path below is only for a terminal view. (The palette-open chord is
        // handled upstream in `handle_key`, before we get here.)
        match &self.main_view {
            MainView::Native(View::Issues) => {
                self.handle_issues_key(k);
                self.dirty = true;
                return;
            }
            MainView::Native(View::Activity) => {
                self.handle_activity_key(k);
                self.dirty = true;
                return;
            }
            MainView::Native(View::Machines) => {
                self.handle_machines_key(k);
                self.dirty = true;
                return;
            }
            _ => {}
        }

        // A scrollback search prompt captures typing.
        if let Some(mut buf) = self.search_input.take() {
            match k.code {
                KeyCode::Esc => {
                    if let Some(p) = self.sessions.live_pane_mut() {
                        p.clear_search();
                    }
                }
                KeyCode::Enter => {
                    if let Some(p) = self.sessions.live_pane_mut() {
                        p.search(&buf);
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                    self.search_input = Some(buf);
                }
                KeyCode::Char(c) => {
                    buf.push(c);
                    self.search_input = Some(buf);
                }
                _ => self.search_input = Some(buf),
            }
            self.dirty = true;
            return;
        }

        // In scrollback, Shelbi owns navigation/search keys.
        let in_scrollback = self
            .sessions
            .live_pane_mut()
            .map(|p| p.in_scrollback())
            .unwrap_or(false);
        if in_scrollback && self.handle_scrollback_key(&k) {
            self.dirty = true;
            return;
        }

        // Shift+PageUp enters scrollback even from the live bottom.
        if k.code == KeyCode::PageUp && k.modifiers.contains(KeyModifiers::SHIFT) {
            if let Some(p) = self.sessions.live_pane_mut() {
                p.scroll_up(p_page());
                self.dirty = true;
                return;
            }
        }

        // Otherwise the key goes to the agent.
        if let Some(p) = self.sessions.live_pane_mut() {
            if let Some(bytes) = p.encode_key(&k) {
                self.sessions.send_input(&bytes);
                self.dirty = true;
            }
        }
    }

    /// Returns `true` if the key was consumed as a scrollback/search command.
    fn handle_scrollback_key(&mut self, k: &KeyEvent) -> bool {
        let Some(p) = self.sessions.live_pane_mut() else {
            return false;
        };
        match k.code {
            KeyCode::Up => p.scroll_up(1),
            KeyCode::Down => p.scroll_down(1),
            KeyCode::PageUp => p.scroll_up(p_page()),
            KeyCode::PageDown => p.scroll_down(p_page()),
            KeyCode::Char('/') => {
                self.search_input = Some(String::new());
            }
            KeyCode::Char('n') => p.search_next(),
            KeyCode::Char('N') => p.search_prev(),
            KeyCode::Esc => {
                p.clear_search();
                p.scroll_to_bottom();
            }
            _ => return false,
        }
        true
    }

    // --- native view key routing -------------------------------------------

    /// Route a key to the embedded issues board (same handler the standalone
    /// `__tasks` process uses, so columns/keys/card actions are identical). The
    /// board's own quit/palette chords are neutralized: the shell owns quit, and
    /// Ctrl+Space is the way back to the sidebar.
    fn handle_issues_key(&mut self, k: KeyEvent) {
        use crate::handlers::kanban::Outcome;
        match crate::handlers::kanban::handle_kanban_key(&mut self.kanban, k, &self.keymaps) {
            // In one process there's no board process to quit, and the palette is
            // a later phase; both are no-ops here.
            Outcome::Quit | Outcome::OpenPalette | Outcome::Continue => {}
        }
    }

    /// Route a key to the embedded activity feed. Quit is neutralized (the shell
    /// owns quit); the feed's scroll/filter/zen chords work as in the standalone.
    fn handle_activity_key(&mut self, k: KeyEvent) {
        crate::handlers::activity::handle_activity_key(&mut self.activity, k, &self.keymaps);
        self.activity.should_quit = false;
    }

    /// Route a key to the embedded machines view. Unlike the standalone process
    /// (which focuses the workspace in a tmux pane), Enter here opens the
    /// workspace's session in the terminal view.
    fn handle_machines_key(&mut self, k: KeyEvent) {
        // Global quit/palette chords are swallowed (the shell owns them).
        let chord = crate::keymap::chord_from_event(k);
        if chord.and_then(|c| self.keymaps.global.dispatch(c)).is_some() {
            return;
        }
        match k.code {
            KeyCode::Up | KeyCode::Char('k') => self.machines.nav_up(),
            KeyCode::Down | KeyCode::Char('j') => self.machines.nav_down(),
            KeyCode::Enter => {
                if let Some(ws) = self.machines.selected_workspace().map(str::to_string) {
                    self.show(RowTarget::Session(SessionRef::Workspace(ws)));
                }
            }
            _ => {}
        }
    }

    fn handle_mouse(&mut self, m: crossterm::event::MouseEvent) {
        if contains(self.sidebar_rect, m.column, m.row) {
            self.handle_sidebar_mouse(m);
            return;
        }
        if contains(self.main_rect, m.column, m.row) {
            self.handle_main_mouse(m);
        }
    }

    fn handle_sidebar_mouse(&mut self, m: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        // Wheel over the sidebar moves the selection.
        match m.kind {
            MouseEventKind::ScrollUp => {
                self.client.focus_sidebar();
                self.client.select_up();
                self.dirty = true;
            }
            MouseEventKind::ScrollDown => {
                let count = self
                    .sidebar_view()
                    .map(|v| v.selectable_count())
                    .unwrap_or(0);
                self.client.focus_sidebar();
                self.client.select_down(count);
                self.dirty = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // A click in the sidebar focuses and selects (plan).
                self.client.focus_sidebar();
                if let Some(view) = self.sidebar_view() {
                    if let Some(sel) = view.hit(self.sidebar_rect, m.column, m.row) {
                        self.client.clamp_selection(view.selectable_count());
                        // Move selection to the clicked row.
                        while self.client.sidebar_selection() < sel {
                            self.client.select_down(view.selectable_count());
                        }
                        while self.client.sidebar_selection() > sel {
                            self.client.select_up();
                        }
                    }
                }
                self.dirty = true;
            }
            _ => {}
        }
    }

    fn handle_main_mouse(&mut self, m: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        // A click in the main area focuses it.
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.client.focus_main();
        }
        // The review interface owns main-area mouse when it is open.
        if matches!(self.main_view, MainView::Review(_)) {
            let area = self.main_rect;
            if let Some(r) = self.review.as_mut() {
                let action = r.handle_mouse(m, area);
                self.apply_review_action(action);
                self.dirty = true;
            }
            return;
        }

        // Native views handle their own mouse. The board's hit maps and the
        // feed's pill hits are recorded in `render_full` against `main_rect`
        // (absolute terminal coordinates), so the standalone handlers' absolute
        // coordinate math applies unchanged.
        match &self.main_view {
            MainView::Native(View::Issues) => {
                crate::handlers::kanban::handle_kanban_mouse(&mut self.kanban, m);
                self.dirty = true;
                return;
            }
            MainView::Native(View::Activity) => {
                crate::handlers::activity::handle_activity_mouse(&mut self.activity, m);
                self.dirty = true;
                return;
            }
            MainView::Native(View::Machines) => {
                match m.kind {
                    MouseEventKind::ScrollUp => self.machines.nav_up(),
                    MouseEventKind::ScrollDown => self.machines.nav_down(),
                    _ => {}
                }
                self.dirty = true;
                return;
            }
            _ => {}
        }

        let origin_col = self.main_rect.x;
        let origin_row = self.main_rect.y;
        let (pane_col, pane_row) = (
            m.column.saturating_sub(origin_col),
            m.row.saturating_sub(origin_row),
        );
        // The viewer the click lands in is the main area; a session sized by
        // another (more-recently-active) client is letterboxed or clipped into
        // it, so the pane translates coordinates against this size.
        let viewer = Size::new(self.main_rect.width, self.main_rect.height);
        if let Some(p) = self.sessions.live_pane_mut() {
            match p.on_mouse(&m, pane_col, pane_row, viewer) {
                terminal_view::MouseOutcome::Forward(bytes) => self.sessions.send_input(&bytes),
                terminal_view::MouseOutcome::Copy(text) => copy_to_clipboard(&text),
                terminal_view::MouseOutcome::Handled => {}
                terminal_view::MouseOutcome::Ignored => {}
            }
            self.dirty = true;
        }
    }

    fn forward_focus(&mut self, focused: bool) {
        if let Some(p) = self.sessions.live_pane_mut() {
            if let Some(bytes) = p.encode_focus(focused) {
                self.sessions.send_input(&bytes);
            }
        }
    }
}

/// A fixed page step for scrollback (rows are re-derived at render; a small
/// constant keeps paging predictable without plumbing the live height here).
fn p_page() -> usize {
    20
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.left() && x < area.right() && y >= area.top() && y < area.bottom()
}

/// Toggle Zen Mode for `project` (a blocking call: it reconciles the daemon
/// version first). Returns a status string; errors are stringified so the
/// worker closure stays self-contained.
fn toggle_zen_blocking(project: &str) -> Result<String, String> {
    shelbi_state::ensure_daemon_matches_for_mutation().map_err(|e| e.to_string())?;
    let state =
        shelbi_state::toggle_zen_mode(project, "user:palette").map_err(|e| e.to_string())?;
    let word = if matches!(state, shelbi_state::ZenModeState::Off) {
        "disabled"
    } else {
        "enabled"
    };
    Ok(format!("Zen Mode {word}"))
}

/// Resolve the inputs for a review panel off the UI thread (board + project
/// config reads). The orchestrator owns the resolution so it matches the
/// daemon's session-spawn path (`rt-tui-review`).
fn resolve_review_open(project: &str, task_id: &str) -> Result<ReviewOpenParams, String> {
    let info = shelbi_orchestrator::review_session::review_open_info(project, task_id)
        .map_err(|e| e.to_string())?;
    Ok(ReviewOpenParams {
        task_id: task_id.to_string(),
        slot: info.slot,
        worktree: info.worktree,
        editor_name: info.editor_name,
        has_review_url: info.has_review_url,
    })
}

/// Run an approve/reject review mutation through the daemon control socket
/// (`shelbi_app::execute_mutation`), stringifying the outcome for the worker.
fn run_review_mutation(m: &shelbi_app::Mutation) -> Result<(), String> {
    shelbi_app::execute_mutation(m, &mut |_s, _t| {}).map_err(|e| e.to_string())
}

/// Whether `task` is still a review-column task on a review slot — the
/// board-derived state a late-attaching (or reconnecting) client lays out from
/// (`review_ui::review_layout_state`). Used to reconcile a stale open review
/// view after a missed `ReviewClosed` (`rt-tui-review`).
fn review_task_still_open(project: &str, task: &str) -> bool {
    match shelbi_orchestrator::review_ui::review_layout_state(project) {
        Ok(slots) => !review_view_is_stale(task, &slots),
        // A transient read error must not yank a live review out from under the
        // user; keep it until a definitive "not in review" read.
        Err(_) => true,
    }
}

/// Whether an open review view for `shown` should be dropped because its task
/// is no longer among the review-column slots `open` — the close half of
/// late-attach reconciliation. `rt-daemon-layout-split`'s
/// [`review_layout_state`](shelbi_orchestrator::review_ui::review_layout_state)
/// reconciled only opens; a `ReviewClosed` missed while this client was
/// detached (or a review that advanced out-of-band) must still drop its view
/// on reconnect (`rt-tui-review`).
fn review_view_is_stale(
    shown: &str,
    open: &[shelbi_orchestrator::review_ui::ReviewSlotLayout],
) -> bool {
    !open.iter().any(|s| s.task == shown)
}

// --- rendering -------------------------------------------------------------

fn draw(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut ShellState,
) -> Result<()> {
    // Compute layout up front so we can report the main-area size to the live
    // session before painting.
    let area = Rect {
        x: 0,
        y: 0,
        width: term.size()?.width,
        height: term.size()?.height,
    };
    let (sidebar_rect, main_rect) = layout(area, state.sidebar_width());
    state.sidebar_rect = sidebar_rect;
    state.main_rect = main_rect;

    // Report our viewport so the session reflows to fill the main area when we
    // are the most-recently-active client.
    let main_size = Size::new(main_rect.width, main_rect.height);
    if state.reported_main != Some(main_size) {
        state.sessions.resize(main_size.cols, main_size.rows);
        // The review content view occupies only the content sub-rect.
        if let Some(r) = state.review.as_ref() {
            r.resize(main_rect);
        }
        state.reported_main = Some(main_size);
    }

    let truecolor = state.caps.truecolor;
    let focus_main = state.focus_is_main();
    let selection = state.selection();
    // The one-time keyboard-protocol notice: shown until its deadline, then
    // cleared for good (so it is emitted exactly once per run).
    let notice = state.notice_text(Instant::now());
    let searching = state.search_input.clone();
    let status = state.status.clone();
    // Take the overlay out so the draw closure can render it with a mutable
    // borrow while it holds shared borrows of the rest of `state`; it is put
    // back right after the draw.
    let mut overlay = state.overlay.take();
    // Taken out like the overlay so the draw closure can render it with a
    // mutable borrow of the frame; put back after the draw (rt-tui-review).
    let mut review = state.review.take();

    // Borrow what the closure needs. `sidebar_view` is owned and `main_view` is
    // cloned (cheap) so the closure can disjointly borrow the embedded views
    // mutably — a native view's `render_full` takes `&mut app` and `&mut Frame`.
    let sidebar_view = state.sidebar_view();
    let main_view = state.main_view.clone();
    let is_review = matches!(main_view, MainView::Review(_));
    let kanban = &mut state.kanban;
    let activity = &mut state.activity;
    let machines = &mut state.machines;
    let sessions = &state.sessions;

    let mut cursor: Option<(u16, u16)> = None;
    let begin = execute!(io::stdout(), BeginSynchronizedUpdate);
    let res = term.draw(|frame| {
        // Native views render through the `Frame` (their own `render_full`), so
        // they must run before the shared buffer is taken below.
        match &main_view {
            MainView::Native(View::Issues) => crate::kanban::render_full(frame, kanban, main_rect),
            MainView::Native(View::Activity) => {
                crate::activity::render_full(frame, activity, main_rect)
            }
            MainView::Native(View::Machines) => {
                crate::machines::render_full(frame, machines, main_rect)
            }
            _ => {}
        }

        // All buffer-level painting happens next, in its own scope, so the `buf`
        // borrow ends before the review interface / overlay re-borrow the frame.
        {
            let buf = frame.buffer_mut();

            // Sidebar.
            if let Some(view) = &sidebar_view {
                view.render(buf, sidebar_rect, selection, !focus_main);
            }

            // Main area (session / review-fallback; native views drawn above).
            match &main_view {
                MainView::Session => match sessions.state() {
                    MainState::Empty => render_placeholder(buf, main_rect, "No session"),
                    MainState::Connecting(r) => {
                        render_placeholder(buf, main_rect, &format!("Connecting to {}…", r.display()))
                    }
                    MainState::Failed(r, err) => render_placeholder(
                        buf,
                        main_rect,
                        &format!("Couldn't attach to {}: {err}", r.display()),
                    ),
                    MainState::Live(pane) => {
                        let cur = pane.render(buf, main_rect, truecolor);
                        if focus_main {
                            cursor = cur;
                        }
                    }
                },
                MainView::Native(_) => {} // drawn above
                MainView::Review(id) => {
                    // The interface itself renders at the frame level below; this
                    // is only the "still opening" fallback before it is built.
                    if review.is_none() {
                        render_placeholder(buf, main_rect, &format!("opening review {id}…"));
                    }
                }
            }

            // The review panel carries its own status line, so the shell's
            // bottom-row status/search band is only for the other main views.
            if !is_review {
                if let Some(q) = &searching {
                    render_search_prompt(buf, main_rect, q);
                } else if let Some(s) = &status {
                    render_status(buf, main_rect, s);
                }
            }

            // One-time keyboard-protocol notice.
            if let Some(text) = notice {
                render_notice(buf, area, text);
            }
        }

        // The review interface (panel + content terminal view) renders with a
        // mutable frame borrow.
        if let Some(r) = review.as_mut() {
            let cur = r.render(frame, main_rect, truecolor, focus_main);
            if overlay.is_none() {
                cursor = cur;
            }
        }

        // Dim the main area under an open overlay so the modal reads as on top
        // (the overlay's own `Clear` un-dims the cells it occupies).
        if overlay.is_some() {
            dim_area(frame.buffer_mut(), main_rect);
        }

        // An open overlay draws over the dimmed main area and owns the cursor.
        if let Some(ov) = overlay.as_mut() {
            ov.render(frame, main_rect);
        } else if let Some((x, y)) = cursor {
            frame.set_cursor_position(Position::new(x, y));
        }
    });
    state.overlay = overlay;
    state.review = review;
    if begin.is_ok() {
        let _ = execute!(io::stdout(), EndSynchronizedUpdate);
    }
    res.context("drawing the shell")?;
    Ok(())
}

/// Dim every cell in `area` (the terminal view behind an open overlay) so the
/// modal on top reads as focused. The overlay's own `Clear` restores full
/// brightness within its rect.
fn dim_area(buf: &mut ratatui::buffer::Buffer, area: Rect) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_style(Style::default().add_modifier(Modifier::DIM));
            }
        }
    }
}

fn render_status(buf: &mut ratatui::buffer::Buffer, area: Rect, text: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let y = area.bottom() - 1;
    let style = Style::default()
        .bg(Color::Rgb(40, 40, 50))
        .fg(Color::Gray);
    let label = format!(" {text} ");
    for (x, ch) in (area.left()..area.right()).zip(label.chars().chain(std::iter::repeat(' '))) {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// Split `area` into (sidebar, main). The sidebar is clamped to leave room for
/// the main area.
fn layout(area: Rect, sidebar_width: u16) -> (Rect, Rect) {
    let max = area.width.saturating_sub(1).max(1);
    let w = sidebar_width.clamp(1, max);
    let sidebar = Rect::new(area.x, area.y, w, area.height);
    let main = Rect::new(area.x + w, area.y, area.width.saturating_sub(w), area.height);
    (sidebar, main)
}

/// The session name the orchestrator chat is bound to in a `View::Session`.
const ORCH_VIEW: &str = "orch";

/// Map a sidebar [`RowTarget`] to the [`View`] it corresponds to, for recording
/// the client's per-project last view. A `Review` target is transient and has
/// no `View`, so it returns `None`.
fn row_target_to_view(target: &RowTarget) -> Option<View> {
    match target {
        RowTarget::Session(SessionRef::Orchestrator) => Some(View::Session(ORCH_VIEW.to_string())),
        RowTarget::Session(SessionRef::Workspace(w)) => Some(View::Session(w.clone())),
        RowTarget::Native(v) => Some(v.clone()),
        // A review content session (editor/diff) is transient and laid out from
        // current state, never restored from a saved view. `rt-tui-review`.
        RowTarget::Session(SessionRef::Review { .. }) | RowTarget::Review(_) => None,
    }
}

/// The inverse of [`row_target_to_view`], for applying a restored view. A
/// `View::Session` names the session to bind (`orch` is the orchestrator chat).
fn view_to_row_target(view: &View) -> RowTarget {
    match view {
        View::Issues | View::Activity | View::Machines => RowTarget::Native(view.clone()),
        View::Session(name) if name == ORCH_VIEW => RowTarget::Session(SessionRef::Orchestrator),
        View::Session(name) => RowTarget::Session(SessionRef::Workspace(name.clone())),
    }
}

fn render_placeholder(buf: &mut ratatui::buffer::Buffer, area: Rect, text: &str) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = Paragraph::new(vec![Line::from(Span::styled(
        text,
        Style::default().fg(Color::DarkGray),
    ))])
    .wrap(Wrap { trim: true });
    p.render(centered(area), buf);
}

/// A small vertically-centered band for placeholder text.
fn centered(area: Rect) -> Rect {
    let y = area.y + area.height / 2;
    Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1)
}

fn render_search_prompt(buf: &mut ratatui::buffer::Buffer, area: Rect, query: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let y = area.bottom() - 1;
    let label = format!(" search: {query}▏");
    let style = Style::default().add_modifier(Modifier::REVERSED);
    for (x, ch) in (area.left()..area.right()).zip(label.chars().chain(std::iter::repeat(' '))) {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

fn render_notice(buf: &mut ratatui::buffer::Buffer, area: Rect, text: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let y = area.top();
    let style = Style::default()
        .bg(Color::Rgb(90, 70, 30))
        .fg(Color::White);
    let label = format!(" {text} ");
    for (x, ch) in (area.left()..area.right()).zip(label.chars()) {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

// --- the background model reader -------------------------------------------

/// Read a fresh snapshot of every view's data for `project`. Runs on the
/// refresher thread (never the UI thread): the sidebar model plus the opaque
/// data bundles the embedded native views fold in. The board read goes through
/// the daemon-owned index (no per-pane `gh` sweep); the activity read is served
/// from the store's TTL cache; machines reads the poller's persisted state. A
/// slow read here never blocks the event loop — the loop only polls
/// [`ShellRefresher::latest`].
fn read_snapshot(project: &str, _generation: u64) -> ShellSnapshot {
    ShellSnapshot {
        sidebar: read_sidebar_model(project),
        board: Some(KanbanApp::read_board_data(project)),
        activity: Some(ActivityApp::read_activity_data(project)),
        machines: Some(MachinesApp::read_data(project)),
    }
}

fn read_sidebar_model(project: &str) -> Option<SidebarModel> {
    let report = shelbi_state::read_board_report(project).ok()?;
    let board = if report.state.is_cold() {
        Vec::new()
    } else {
        report.state.into_issues()
    };
    let names = workspace_pool(project);
    let zen_on = shelbi_state::read_state(project)
        .map(|s| !matches!(s.zen_mode, shelbi_state::ZenModeState::Off))
        .unwrap_or(false);
    let unread = shelbi_state::unread_error_count(project).unwrap_or(0);
    Some(SidebarModel::from_board(project, &board, &names, zen_on, unread))
}

/// The project's declared dev-workspace pool (review-tagged slots surface only
/// through the review sections, mirroring the tmux sidebar).
fn workspace_pool(project: &str) -> Vec<String> {
    let Ok(p) = shelbi_state::load_project(project) else {
        return Vec::new();
    };
    p.workspaces
        .iter()
        .filter(|w| !p.effective_tags(w).contains("review"))
        .map(|w| w.name.clone())
        .collect()
}

// --- clipboard -------------------------------------------------------------

/// Copy `text` to the clipboard. OSC 52 is the portable path (it works over SSH
/// and through an outer tmux/Screen when configured); locally we also pipe to
/// the native clipboard as a best-effort fallback.
fn copy_to_clipboard(text: &str) {
    let mut out = io::stdout();
    let _ = out.write_all(&terminal_view::osc52(text));
    let _ = out.flush();
    native_clipboard(text);
}

#[cfg(target_os = "macos")]
fn native_clipboard(text: &str) {
    use std::process::{Command, Stdio};
    if let Ok(mut child) = Command::new("pbcopy").stdin(Stdio::piped()).spawn() {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
    }
}

#[cfg(not(target_os = "macos"))]
fn native_clipboard(_text: &str) {
    // On Linux the OSC 52 path is the primary; a native fallback (wl-copy /
    // xclip) is deferred — it needs display-server detection that is out of
    // scope for Phase 4b.
}

// --- terminal lifecycle ----------------------------------------------------

/// Best-effort terminal restore, safe from the panic hook, the RAII guard, and
/// an explicit call.
fn restore_terminal() {
    let mut out = io::stdout();
    let _ = execute!(
        out,
        PopKeyboardEnhancementFlags,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        LeaveAlternateScreen,
        Show,
    );
    let _ = disable_raw_mode();
    let _ = out.flush();
}

/// RAII terminal setup: raw mode, alternate screen, mouse/focus/paste capture,
/// and a best-effort kitty keyboard push so Shift+Enter reaches the agent where
/// the terminal supports it. Restores everything on drop and on panic.
struct RawGuard;

impl RawGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut out = io::stdout();
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableFocusChange,
            EnableBracketedPaste,
        )?;
        let _ = execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            prev(info);
        }));
        Ok(RawGuard)
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_app::view::{NavItem, WorkspaceRow};

    struct NoopConnector;
    impl session::Connector for NoopConnector {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<session::Connected, String> {
            Err("no session in tests".into())
        }
    }

    fn test_state() -> ShellState {
        let caps = Caps {
            kitty: true,
            truecolor: true,
            nested: None,
        };
        let mut st = ShellState::new("proj", Arc::new(NoopConnector), caps);
        st.apply_snapshot(ShellSnapshot {
            sidebar: Some(SidebarModel {
                project_label: "proj".into(),
                nav: vec![
                    NavItem {
                        label: "Chat".into(),
                        view: View::Session("orch".into()),
                    },
                    NavItem {
                        label: "Issues".into(),
                        view: View::Issues,
                    },
                    NavItem {
                        label: "Activity".into(),
                        view: View::Activity,
                    },
                ],
                workspaces: vec![WorkspaceRow {
                    name: "alpha".into(),
                    current_task: None,
                    agent: None,
                }],
                reviews: vec![],
                zen_on: false,
                unread_errors: 0,
            }),
            board: None,
            activity: None,
            machines: None,
        });
        st
    }

    #[test]
    fn ctrl_space_opens_the_palette_from_the_terminal_view() {
        // rt-tui-overlays replaces the interim focus-toggle: Ctrl+Space from a
        // focused terminal view opens the command palette overlay.
        let mut st = test_state();
        st.client.focus_main();
        assert!(st.overlay.is_none());
        st.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert!(
            matches!(st.overlay, Some(ActiveOverlay::Palette(_))),
            "Ctrl+Space opens the palette"
        );
    }

    #[test]
    fn ctrl_space_opens_the_palette_from_the_sidebar_too() {
        let mut st = test_state();
        st.client.focus_sidebar();
        st.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert!(matches!(st.overlay, Some(ActiveOverlay::Palette(_))));
    }

    #[test]
    fn palette_esc_returns_to_the_agent_and_tab_focuses_the_sidebar() {
        // Esc closes the palette and returns focus to the main terminal view.
        let mut st = test_state();
        st.client.focus_main();
        st.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert!(st.overlay.is_some());
        st.handle_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(st.overlay.is_none(), "Esc closes the palette");
        assert!(st.focus_is_main(), "focus returns to the agent");

        // Tab closes the palette and moves focus to the sidebar.
        st.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert!(st.overlay.is_some());
        st.handle_event(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        assert!(st.overlay.is_none(), "Tab closes the palette");
        assert_eq!(st.client.focus(), Focus::Sidebar, "Tab moves focus to the sidebar");
    }

    #[test]
    fn palette_lists_the_registry_commands_from_the_sidebar_model() {
        // The open palette's entries come from the shelbi-app command registry,
        // built from the shell's sidebar model — so the nav views, the Zen
        // toggle, workspaces, and the error-log action are all reachable.
        let mut st = test_state();
        st.open_palette();
        let Some(ActiveOverlay::Palette(p)) = &st.overlay else {
            panic!("palette should be open");
        };
        let ids: Vec<String> = p.results().into_iter().map(|(e, _)| e.id).collect();
        assert!(ids.contains(&"view:tasks".to_string()), "Issues view reachable");
        assert!(ids.contains(&"action:toggle-zen".to_string()), "Zen toggle reachable");
        assert!(ids.contains(&"workspace:alpha".to_string()), "workspace reachable");
        assert!(ids.contains(&"action:error-log".to_string()), "error log reachable");
    }

    #[test]
    fn sidebar_arrows_tab_and_enter_navigate_and_activate() {
        let mut st = test_state();
        st.client.focus_sidebar();
        assert_eq!(st.selection(), 0, "starts on Chat");
        st.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(st.selection(), 1, "Down → Issues");
        st.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(st.selection(), 2, "Tab → Activity");
        // Enter on a native view swaps the main area and keeps sidebar focus.
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Native(View::Activity)));
        assert_eq!(st.client.focus(), Focus::Sidebar);
    }

    /// A sidebar with every native-view nav entry (Chat/Issues/Activity/Machines),
    /// one workspace, and no reviews.
    fn state_with_machines_nav() -> ShellState {
        let caps = Caps { kitty: true, truecolor: true, nested: None };
        let mut st = ShellState::new("proj", Arc::new(NoopConnector), caps);
        st.apply_snapshot(ShellSnapshot {
            sidebar: Some(SidebarModel {
                project_label: "proj".into(),
                nav: vec![
                    NavItem { label: "Chat".into(), view: View::Session("orch".into()) },
                    NavItem { label: "Issues".into(), view: View::Issues },
                    NavItem { label: "Activity".into(), view: View::Activity },
                    NavItem { label: "Machines".into(), view: View::Machines },
                ],
                workspaces: vec![WorkspaceRow { name: "alpha".into(), current_task: None, agent: None }],
                reviews: vec![],
                zen_on: false,
                unread_errors: 0,
            }),
            board: None,
            activity: None,
            machines: None,
        });
        st
    }

    #[test]
    fn issues_activity_and_machines_open_in_the_main_area_from_the_sidebar() {
        let mut st = state_with_machines_nav();
        st.client.focus_sidebar();
        // Rows: 0 Chat, 1 Issues, 2 Activity, 3 Machines, 4 alpha.
        st.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // Issues
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Native(View::Issues)));
        assert_eq!(st.client.view(), &View::Issues, "the view is recorded for the project");

        st.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // Activity
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Native(View::Activity)));

        st.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // Machines
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Native(View::Machines)));
        assert_eq!(st.client.view(), &View::Machines);
    }

    #[test]
    fn the_embedded_board_routes_moves_through_the_executor() {
        // The shell installs the executor-routing move persister so board moves go
        // through the shelbi-app executor (daemon-backed when the setting is on);
        // the standalone process keeps its default direct path (move_persister None).
        let st = state_with_machines_nav();
        assert!(
            st.kanban.move_persister.is_some(),
            "the shell's board moves route through the executor"
        );
        assert!(
            KanbanApp::new("proj").move_persister.is_none(),
            "the standalone board keeps its direct library path"
        );
    }

    #[test]
    fn the_last_view_is_restored_when_switching_back_to_a_project() {
        let mut st = state_with_machines_nav();
        // Open Activity on this project; it is recorded as the project's view.
        st.show(RowTarget::Native(View::Activity));
        assert_eq!(st.client.view(), &View::Activity);
        // Switch away to another project (lands on that project's default view)…
        st.client.switch_project("other");
        assert_eq!(st.client.view(), &View::default_for_project());
        // …and back: the Activity view is restored.
        st.client.switch_project("proj");
        assert_eq!(st.client.view(), &View::Activity);
    }

    #[test]
    fn machines_enter_opens_the_selected_workspace_session() {
        use crate::machines::{MachineEntry, MachinesData, WorkspaceRow as MachineWsRow};
        use shelbi_core::MachineKind;

        let mut st = state_with_machines_nav();
        // Feed the machines view one machine with one workspace.
        st.machines.apply_data(MachinesData {
            display_name: Some("proj".into()),
            machines: vec![MachineEntry {
                name: "local".into(),
                kind: MachineKind::Local,
                host: None,
                is_local: true,
                tags: vec![],
                remote: None,
                reachability: shelbi_orchestrator::machine::Reachability::Reachable,
                workspaces: vec![MachineWsRow { name: "alpha".into(), state: None, current_task: None }],
            }],
        });
        st.main_view = MainView::Native(View::Machines);
        st.client.focus_main();
        // Enter on the selected workspace opens its session in the terminal view.
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Session));
        assert_eq!(
            st.sessions.current_target(),
            Some(&SessionRef::Workspace("alpha".into()))
        );
    }

    #[test]
    fn a_key_in_the_issues_view_routes_to_the_board_not_the_session() {
        // With the board focused in the main area, a board affordance (`f` opens
        // the workspace filter dropdown) must reach the embedded KanbanApp rather
        // than being sent to a session. `f` is routed directly by the kanban
        // handler (not a keymap binding), so this is independent of `keys.yaml`;
        // the lock+temp-home keep construction deterministic anyway.
        let _g = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = std::env::temp_dir().join(format!(
            "shelbi-shell-issues-route-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let caps = Caps { kitty: true, truecolor: true, nested: None };
        let mut st = ShellState::new("proj", Arc::new(NoopConnector), caps);
        st.main_view = MainView::Native(View::Issues);
        st.client.focus_main();
        assert!(!st.kanban.workspace_dropdown_is_open());
        st.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
        assert!(
            st.kanban.workspace_dropdown_is_open(),
            "the key reached the board handler"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn activating_a_workspace_shows_its_session_and_focuses_main() {
        let mut st = test_state();
        st.client.focus_sidebar();
        // Rows: 0 Chat, 1 Issues, 2 Activity, 3 alpha (workspace).
        for _ in 0..3 {
            st.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(st.selection(), 3);
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Session));
        assert!(st.focus_is_main(), "opening a session focuses the main area");
        assert_eq!(
            st.sessions.current_target(),
            Some(&SessionRef::Workspace("alpha".into()))
        );
    }

    #[test]
    fn close_reconcile_drops_a_review_whose_task_left_the_column() {
        use shelbi_orchestrator::review_ui::ReviewSlotLayout;
        let open = vec![ReviewSlotLayout {
            workspace: "review-1".into(),
            task: "t-a".into(),
        }];
        // A review whose task is still on a review slot stays.
        assert!(!review_view_is_stale("t-a", &open));
        // A review whose task has left the review column is dropped — the
        // close-reconcile the open-only layout events missed (rt-tui-review).
        assert!(review_view_is_stale("t-gone", &open));
        // Nothing open at all → any shown review is stale.
        assert!(review_view_is_stale("t-a", &[]));
    }

    #[test]
    fn layout_splits_sidebar_and_main() {
        let area = Rect::new(0, 0, 100, 40);
        let (sb, main) = layout(area, 28);
        assert_eq!(sb, Rect::new(0, 0, 28, 40));
        assert_eq!(main, Rect::new(28, 0, 72, 40));
    }

    #[test]
    fn layout_clamps_a_too_wide_sidebar() {
        let area = Rect::new(0, 0, 10, 5);
        let (sb, main) = layout(area, 28);
        assert_eq!(sb.width, 9, "sidebar leaves at least one column for main");
        assert_eq!(main.width, 1);
    }

    #[test]
    fn keyboard_notice_is_emitted_exactly_once() {
        // Inside tmux with no kitty-protocol round-trip (the shape AC9 names).
        let caps = Caps {
            kitty: false,
            truecolor: true,
            nested: Some(caps::Nesting::Tmux),
        };
        let mut st = ShellState::new("proj", Arc::new(NoopConnector), caps);
        // Armed at startup; within the window it paints the tmux-tailored notice.
        let now = Instant::now();
        let text = st.notice_text(now).expect("notice is armed without the kitty protocol");
        assert!(text.contains("extended-keys"), "the tmux fix is named: {text}");
        assert!(st.notice_text(now).is_some(), "still showing within the window");
        // Past the deadline it clears...
        let later = now + Duration::from_secs(NOTICE_SECS + 1);
        assert!(st.notice_text(later).is_none(), "the notice hides after its deadline");
        // ...and never re-arms, even if queried again with an earlier instant:
        // the banner is emitted exactly once per run.
        assert!(st.notice_text(now).is_none(), "the notice is never shown a second time");
    }

    #[test]
    fn no_keyboard_notice_when_the_protocol_round_trips() {
        let caps = Caps {
            kitty: true,
            truecolor: true,
            nested: Some(caps::Nesting::Tmux),
        };
        let mut st = ShellState::new("proj", Arc::new(NoopConnector), caps);
        assert!(st.notice_text(Instant::now()).is_none(), "kitty present → no notice");
    }

    #[test]
    fn is_actionable_ignores_releases() {
        use crossterm::event::{KeyEventKind, KeyEventState};
        let press = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(terminal_view::is_actionable(&press));
        let release = KeyEvent {
            code: KeyCode::Char('a'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
        };
        assert!(!terminal_view::is_actionable(&release));
    }

    // --- Phase 4f: project switching, quit actions, re-exec ------------------

    use std::sync::Mutex;

    /// Records the project/shelbi quit calls the shell makes.
    #[derive(Default)]
    struct RecordingLifecycle {
        calls: Mutex<Vec<String>>,
    }
    impl ShellLifecycle for RecordingLifecycle {
        fn quit_project(&self, project: &str) {
            self.calls.lock().unwrap().push(format!("quit_project:{project}"));
        }
        fn quit_shelbi(&self) {
            self.calls.lock().unwrap().push("quit_shelbi".into());
        }
    }

    fn test_state_with(lifecycle: Arc<RecordingLifecycle>) -> ShellState {
        let caps = Caps {
            kitty: true,
            truecolor: true,
            nested: None,
        };
        ShellState::new_with("proj", Arc::new(NoopConnector), caps, lifecycle)
    }

    #[test]
    fn view_target_mapping_round_trips() {
        for view in [
            View::Issues,
            View::Activity,
            View::Machines,
            View::Session("orch".into()),
            View::Session("alpha".into()),
        ] {
            let rt = view_to_row_target(&view);
            assert_eq!(row_target_to_view(&rt), Some(view));
        }
        // The orchestrator chat maps to the Orchestrator session ref.
        assert_eq!(
            view_to_row_target(&View::Session("orch".into())),
            RowTarget::Session(SessionRef::Orchestrator)
        );
    }

    #[test]
    fn switching_projects_restores_each_projects_last_view() {
        // AC1: switching projects restores each project's last view for this
        // client. Drive the shell as a user would: open views, switch away,
        // switch back, and confirm the main area lands where it was left.
        let mut st = test_state_with(Arc::new(RecordingLifecycle::default()));
        // On `proj`, open the Activity view.
        st.show(RowTarget::Native(View::Activity));
        assert!(matches!(st.main_view, MainView::Native(View::Activity)));

        // Switch to `beta`: lands on beta's default (Issues), not proj's.
        st.switch_project("beta");
        assert_eq!(st.client.project(), Some("beta"));
        assert!(matches!(st.main_view, MainView::Native(View::Issues)));
        // On beta, open Machines.
        st.show(RowTarget::Native(View::Machines));

        // Back to proj: the Activity view is restored.
        st.switch_project("proj");
        assert!(
            matches!(st.main_view, MainView::Native(View::Activity)),
            "proj's last view (Activity) must be restored"
        );
        // Back to beta: Machines is restored.
        st.switch_project("beta");
        assert!(matches!(st.main_view, MainView::Native(View::Machines)));
    }

    #[test]
    fn close_ui_quits_without_touching_sessions() {
        // AC2: `q` closes the UI and every session keeps running. The close-UI
        // path must invoke NO lifecycle (quit-project/quit-shelbi) operation —
        // that is what leaves the agents alive for a reopen to reattach to.
        let life = Arc::new(RecordingLifecycle::default());
        let mut st = test_state_with(life.clone());
        st.quit(QuitAction::CloseUi);
        assert!(st.should_quit, "close-UI ends the loop");
        assert!(!st.should_reexec);
        assert!(
            life.calls.lock().unwrap().is_empty(),
            "close-UI must not end any sessions: {:?}",
            life.calls.lock().unwrap()
        );
    }

    #[test]
    fn q_in_the_sidebar_closes_the_ui() {
        let life = Arc::new(RecordingLifecycle::default());
        let mut st = test_state_with(life.clone());
        st.client.focus_sidebar();
        st.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(st.should_quit);
        assert!(life.calls.lock().unwrap().is_empty(), "`q` is close-UI, not a quit-project");
    }

    #[test]
    fn quit_project_and_quit_shelbi_route_through_the_lifecycle() {
        let life = Arc::new(RecordingLifecycle::default());
        let mut st = test_state_with(life.clone());
        st.quit(QuitAction::QuitProject);
        assert!(st.should_quit);
        st.should_quit = false;
        st.quit(QuitAction::QuitShelbi);
        assert_eq!(
            *life.calls.lock().unwrap(),
            vec!["quit_project:proj".to_string(), "quit_shelbi".to_string()],
        );
    }

    #[test]
    fn palette_quit_project_reaches_the_daemon_command() {
        // Rework: selecting "Quit project" in the command palette must reach the
        // daemon lifecycle command, not just set a status note. Drive the exact
        // path the palette takes when a user runs that entry — `run_entry`
        // resolves the id to its command effect and dispatches it — and confirm
        // it lands on the lifecycle seam (the daemon control socket in
        // production). The id↔kind mapping is owned by `shelbi_app::command`.
        let life = Arc::new(RecordingLifecycle::default());
        let mut st = test_state_with(life.clone());
        st.run_entry(&shelbi_palette::Entry {
            id: "action:quit-project".to_string(),
            label: "Quit project".to_string(),
            kind: shelbi_palette::EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: false,
        });
        assert!(st.should_quit, "a palette quit-project ends the loop");
        assert_eq!(
            *life.calls.lock().unwrap(),
            vec!["quit_project:proj".to_string()],
            "the palette's quit-project must reach the daemon lifecycle command",
        );
    }
}
