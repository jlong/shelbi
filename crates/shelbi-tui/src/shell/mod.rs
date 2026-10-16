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
mod overlays;
mod refresh;
// The relaunchers moved to `shelbi_app::session::relaunch` (shared with the
// desktop app).
use shelbi_app::session::relaunch;
mod review;
mod session;
mod sidebar;
mod workspace;
mod terminal_view;

#[cfg(all(test, unix))]
mod pty_input_tests;

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
use shelbi_state::keymap::{load_keymaps, DisplayStyle, GlobalAction, KeyChord, Keymaps};
use shelbi_term::Size;

use crate::theme::palette;
use crate::activity::{ActivityApp, ActivityData, ActivityMetaCache};
use crate::kanban::{ClosedPageRead, ExecutorMovePersister, KanbanApp};
use crate::machines::MachinesApp;
use caps::Caps;
use overlays::{ActiveOverlay, OverlayEvent};
use refresh::ShellSnapshot;
use review::{ReviewAction, ReviewInterface};
use session::{gave_up_notice, LiveConnector, MainState, SessionManager, SessionRef};
use sidebar::{RowTarget, SidebarView};
use workspace::{WorkspaceAction, WorkspaceInterface};

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
    /// The reviewed task, for the panel's title / description preview and the
    /// full-description popover (`None` when the store couldn't produce it).
    /// Boxed — it's large — so [`ResolvedReview`]'s variants stay balanced.
    task: Option<Box<shelbi_state::IssueFile>>,
}

/// How a review-open resolve landed off the UI thread (`rt-tui-review-load-queued`):
/// the task is already on a slot (open the interface) or still queued (pick a
/// free slot to load it onto).
enum ResolvedReview {
    /// Already serving — build the native interface with these inputs.
    Serving(ReviewOpenParams),
    /// Queued — offer the free review slots. `total_slots` lets the shell tell
    /// "every slot is busy" (some exist, none free) from "no review workspace"
    /// (none exist). `title` is the card title the picker shows.
    Queued {
        title: String,
        free_slots: Vec<crate::overlay::review_confirm::Slot>,
        total_slots: usize,
    },
}

/// The daemon-facing review operations the shell runs off its UI thread, behind
/// a trait so the queued-load flow is testable with a stubbed daemon
/// (`rt-tui-review-load-queued`).
trait ReviewBackend: Send + Sync {
    /// Resolve how to open `task`: serving on a slot, or queued with the free
    /// slots to pick from. Blocking board/config reads.
    fn resolve(&self, project: &str, task: &str) -> Result<ResolvedReview, String>;
    /// Load a queued `task` onto the free review `workspace` through the daemon
    /// (checkout + boot + health-check + agent). Blocking.
    fn load(&self, project: &str, task: &str, workspace: &str) -> Result<(), String>;
}

/// The production [`ReviewBackend`]: resolve through the orchestrator's
/// board-derived state and load through the daemon control socket.
struct DaemonReviewBackend;

impl ReviewBackend for DaemonReviewBackend {
    fn resolve(&self, project: &str, task: &str) -> Result<ResolvedReview, String> {
        use shelbi_orchestrator::review_session::ReviewOpenTarget;
        match shelbi_orchestrator::review_session::review_open_target(project, task)
            .map_err(|e| e.to_string())?
        {
            ReviewOpenTarget::Serving(info) => Ok(ResolvedReview::Serving(ReviewOpenParams {
                task_id: task.to_string(),
                slot: info.slot,
                worktree: info.worktree,
                editor_name: info.editor_name,
                has_review_url: info.has_review_url,
                task: info.task,
            })),
            ReviewOpenTarget::Queued { title } => {
                // List the free review slots (the only loadable ones here — a
                // busy slot is left for its occupant, not evicted), plus the
                // total so the shell can report "every slot is busy".
                let all = shelbi_orchestrator::load::review_slots(project)
                    .map_err(|e| e.to_string())?;
                let total_slots = all.len();
                let free_slots = all
                    .into_iter()
                    .filter(|s| s.occupant.is_none())
                    .map(|s| crate::overlay::review_confirm::Slot {
                        name: s.name,
                        occupant: None,
                    })
                    .collect();
                Ok(ResolvedReview::Queued {
                    title,
                    free_slots,
                    total_slots,
                })
            }
        }
    }

    fn load(&self, project: &str, task: &str, workspace: &str) -> Result<(), String> {
        shelbi_app::review_session(
            project,
            task,
            shelbi_app::ReviewSessionOp::Load {
                workspace: workspace.to_string(),
            },
            &mut |_, _| {},
        )
        .map_err(|e| e.to_string())
    }
}

/// A message from a review background job (all run off the UI thread).
enum ReviewJobMsg {
    /// A review-open resolve finished: build the interface, open the slot
    /// picker, or show the error (`rt-tui-review-load-queued`).
    Resolved(Result<ResolvedReview, String>),
    /// A queued-review load onto a slot finished (`rt-tui-review-load-queued`).
    /// On success the daemon's `ReviewOpened` event opens the interface.
    LoadDone(Result<(), String>),
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
/// The status line shown while the off-thread startup (daemon + dashboard
/// bootstrap) runs, cleared when it succeeds (`rt-tui-headless-startup-block`).
const STARTUP_STATUS: &str = "starting orchestrator…";

/// Run the single-process shell for `project` until the user quits.
pub fn run(project: &str) -> Result<()> {
    run_with(project, Arc::new(LiveConnector))
}

/// Run the shell against an injected connector (for tests that avoid a real
/// session). The terminal setup still happens, so this needs a TTY; the pure
/// routing is covered by unit tests instead.
fn run_with(project: &str, connector: Arc<dyn session::Connector>) -> Result<()> {
    // Capabilities split in two so the first frame never waits on a terminal
    // query (`rt-tui-headless-startup-block`): the env-derived signals
    // (truecolor, nesting) are instant and used to build the shell now; the
    // kitty round-trip is probed *after* the first draw, below.
    let caps = Caps::detect_fast();
    // The color theme, chosen once from `config.yaml::theme` before the first
    // frame (`auto` is dark: a terminal doesn't report its appearance).
    crate::theme::install_from_config();

    let _guard = RawGuard::enter().context("entering raw mode")?;
    let mut term =
        Terminal::new(CrosstermBackend::new(io::stdout())).context("initializing the terminal")?;

    // Build with the keyboard-protocol notice suppressed (a placeholder
    // `kitty: true`): the post-first-frame probe below establishes the real
    // value and arms the notice through `set_caps`, so a kitty-capable terminal
    // never flashes the banner on frame one. `kitty` drives only that notice, so
    // the placeholder doesn't affect the first frame's rendering.
    let mut state = ShellState::new(project, connector, Caps { kitty: true, ..caps });
    // Start the daemon + dashboard off the UI thread, and show a status note
    // while it runs. The first frame draws immediately (below), before any of
    // this completes.
    state.status = Some(STARTUP_STATUS.to_string());
    state.spawn_startup();

    // React to the daemon poller's layout events (review opened/closed/agent
    // recovered), with this process's own change bus as the setting-off
    // fallback — the same two sources the tmux sidebar drains
    // (`rt-daemon-layout-split`), now driving the native review interface. The
    // bus is project-agnostic (filtered per tick); the hub-socket layout
    // subscriber is part of the per-project worker bundle below.
    state.layout_bus = Some(shelbi_state::subscribe_changes());

    // Spawn the per-project background workers (snapshot + activity refreshers +
    // the change and layout hub-socket subscribers) for the launch project, and
    // request an initial refresh. A project switch rebuilds this bundle for the new project
    // (`ShellState::switch_project`), so every native view follows the current
    // project rather than staying pinned to the launch one.
    state.start_project_workers();
    let mut last_refresh = Instant::now();

    // Daemon-push listener: a background subscription — registered as this
    // attached client — that flips `reexec` on a re-exec push (this client is out
    // of date after an upgrade, or `shelbi reload` signalled a re-exec) and the
    // detach signal on a detach push (`shelbi detach` / another window's "Detach
    // Other Clients"). The loop observes both flags and exits accordingly.
    let reexec = Arc::new(AtomicBool::new(false));
    let detach_signal = state.detach_signal.clone();
    let client_info = shelbi_proto::control::ClientInfo {
        client_id: state.client_id.clone(),
        kind: shelbi_proto::control::ClientKind::Tui,
        host: shelbi_client::hostname(),
        pid: Some(std::process::id()),
        project: Some(project.to_string()),
    };
    spawn_reexec_listener(reexec.clone(), detach_signal, client_info);

    // Attached-client count poller: keeps the "other clients attached" flag the
    // palette gates "Detach Other Clients" on fresh. Stopped on loop exit.
    let count_stop = Arc::new(AtomicBool::new(false));
    spawn_client_count_poller(
        state.other_clients.clone(),
        state.client_id.clone(),
        count_stop.clone(),
    );

    // Start on the restored view (set by a prior re-exec) or the orchestrator
    // chat, focused.
    match reexec_restored_view() {
        Some(view) => state.apply_restored_view(view),
        None => state.show(RowTarget::Session(SessionRef::Orchestrator)),
    }

    // Draw the first frame now — the sidebar shell with placeholders and the
    // "starting orchestrator…" note — *before* probing the terminal or waiting
    // on the daemon, so a headless PTY sees a frame within milliseconds instead
    // of a multi-second blank (`rt-tui-headless-startup-block`).
    draw(&mut term, &mut state)?;
    let mut last_draw = Instant::now();

    // Read every project's config-only sidebar skeleton in the background, so a
    // later cold Switch Project paints the target's workspace rows on its very
    // first frame without touching disk on the UI thread.
    state.spawn_skeleton_prefetch();

    // Now probe the kitty keyboard protocol with a short timeout (the terminal
    // is in raw mode and nothing else is draining stdin yet). A headless PTY
    // that never answers falls back to `kitty: false` within the budget rather
    // than blocking; the result arms the one-time keyboard notice.
    let kitty = caps::probe_keyboard_enhancement(caps::KITTY_PROBE_TIMEOUT);
    state.set_caps(Caps { kitty, ..caps });

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
        // A pushed detach ends the loop cleanly (CloseUi semantics): sessions and
        // agents keep running, and the reason is printed after the terminal is
        // restored (below).
        if state.detach_signal.is_requested() && !state.should_quit {
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

        // An `edit:*` command asked for the user's editor: hand it the terminal
        // and wait (the editor owns the screen meanwhile), then repaint.
        if let Some(path) = state.pending_editor.take() {
            state.status = Some(run_editor(&mut term, &path));
            state.reported_main = None;
            state.dirty = true;
        }

        // Background sources (all non-blocking). A daemon change notification
        // triggers an immediate refresh; the periodic request is the fallback.
        // These read the current project's worker bundle, which a project switch
        // replaces, so after a switch they follow the new project.
        let (changed, woke_workspaces) = state.drain_change_wakes();
        if changed || last_refresh.elapsed() >= REFRESH_INTERVAL {
            // Drives both the core snapshot refresher and the activity refresher
            // (on the same cadence, so the activity feed's incremental title
            // hydration keeps making progress without ever gating the core
            // snapshot). Both live in the current project's worker bundle, so
            // after a switch they follow the new project.
            state.request_refresh();
            last_refresh = Instant::now();
        }
        if let Some(snap) = state.latest_snapshot() {
            state.apply_snapshot(snap);
        }
        if let Some(activity) = state.latest_activity() {
            state.activity.apply_activity_data(activity);
            state.dirty = true;
        }
        // The terminal done/canceled history fills in on its own refresher, after
        // (and independent of) the open board, so a stale-cache GraphQL fetch
        // never delays the board's first paint.
        if let Some(closed) = state.latest_closed() {
            state.kanban.apply_closed_page(closed);
            state.dirty = true;
        }
        // A change arrived for the workspace the main area is showing: if its
        // attach was idle or failed (no session yet), retry so a session that
        // just started shows up without the user re-selecting the row
        // (`rt-tui-idle-workspace-open`). Scoped to the shown workspace so
        // unrelated board churn never flickers the placeholder.
        if state.retry_shown_workspace(&woke_workspaces) {
            state.dirty = true;
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
        // The workspace interface's content terminal view runs its own connection.
        if let Some(w) = state.workspace.as_mut() {
            if w.pump_output(&mut ring) {
                state.dirty = true;
            }
            if w.poll() {
                state.dirty = true;
            }
        }
        if state.poll_workspace() {
            state.dirty = true;
        }
        if state.poll_job() {
            state.dirty = true;
        }
        if state.poll_create_job() {
            state.dirty = true;
        }
        if state.poll_startup() {
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

    // Stop the background client-count poller (its thread exits on the next tick).
    count_stop.store(true, Ordering::SeqCst);

    // Decide the on-exit action(s) before touching the terminal: a daemon-asked
    // re-exec (carry the view forward and replace the process) and/or a daemon
    // detach (print why the TUI closed on the normal screen). They are mutually
    // exclusive in practice, but computing both keeps the one terminal teardown
    // below unconditional for the borrow checker.
    let reexec_view = state.should_reexec.then(|| state.client.view().clone());
    let detach_reason = state.detach_signal.take_reason();

    // Restore the terminal once if either path needs the normal screen. (The RAII
    // guard restores on drop anyway, but `exec` replaces the process so Drop never
    // runs, and a detach message must print after the restore.)
    if reexec_view.is_some() || detach_reason.is_some() {
        drop(term);
        drop(_guard);
    }

    if let Some(view) = reexec_view {
        reexec_into_current_binary(&view);
        // Only returns if exec failed; fall through to a clean exit so the shell
        // doesn't hang in a broken terminal.
    }

    // The daemon detached this client: sessions and agents keep running, and a
    // plain `shelbi` reattaches.
    if let Some(reason) = detach_reason {
        println!("{reason}");
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

/// A daemon-pushed request for this client to detach (`shelbi detach` / the
/// palette's "Detach Other Clients" from another window). The background listener
/// sets `requested` and records the one-line `reason` the shell prints on exit.
#[derive(Default)]
struct DetachSignal {
    requested: AtomicBool,
    reason: Mutex<Option<String>>,
}

impl DetachSignal {
    fn request(&self, reason: String) {
        *self.reason.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason);
        self.requested.store(true, Ordering::SeqCst);
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Take the recorded reason (consumed once, for the on-exit message).
    fn take_reason(&self) -> Option<String> {
        self.reason.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

/// Spawn the background daemon-push listener: subscribe to the daemon's control
/// socket **as this attached client** (so `shelbi detach` can list and target
/// it) and react to the pushes — flip `reexec` on a [`Notice::Reexec`], flip
/// `detach` on a [`Notice::Detach`] (recording its reason). If the subscription
/// drops (the daemon restarted), re-probe the daemon version: a mismatch means
/// this client is now out of date, so flip the re-exec flag; otherwise reconnect
/// (re-registering under the same `client_id`). All best-effort — a daemon that
/// is simply absent leaves both flags clear, and the shell keeps running.
fn spawn_reexec_listener(
    reexec: Arc<AtomicBool>,
    detach: Arc<DetachSignal>,
    info: shelbi_proto::control::ClientInfo,
) {
    std::thread::Builder::new()
        .name("shelbi-shell-daemon-push".into())
        .spawn(move || loop {
            if reexec.load(Ordering::SeqCst) || detach.is_requested() {
                return;
            }
            match connect_control() {
                Some(client) => {
                    let Ok(mut sub) = client.subscribe_as(info.clone()) else {
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    };
                    loop {
                        match sub.recv() {
                            Ok(Some(shelbi_client::Notice::Reexec { .. })) => {
                                reexec.store(true, Ordering::SeqCst);
                                return;
                            }
                            Ok(Some(shelbi_client::Notice::Detach { reason })) => {
                                detach.request(reason);
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

/// Spawn the background attached-client poller: every couple of seconds, list the
/// attached clients and store how many are *other* than this one in `others`, so
/// the palette can offer "Detach Other Clients" only when there is one. Purely
/// best-effort — an unreachable daemon leaves the count at zero.
fn spawn_client_count_poller(others: Arc<AtomicUsize>, my_id: String, stop: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("shelbi-shell-client-count".into())
        .spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let count = match connect_control() {
                Some(mut client) => client
                    .list_clients()
                    .map(|clients| clients.iter().filter(|c| c.client_id != my_id).count())
                    .unwrap_or(0),
                None => 0,
            };
            others.store(count, Ordering::SeqCst);
            std::thread::sleep(Duration::from_secs(2));
        })
        .ok();
}

/// What the main area is currently showing.
#[derive(Clone)]
enum MainView {
    Session,
    Native(View),
    Review(String),
    /// The workspace interface (panel + content view) for the named dev
    /// workspace (the workspace-sidebar task). Like [`MainView::Review`] it is a
    /// transient interface, not a remembered view.
    Workspace(String),
}

/// Resolved inputs the shell needs to build a [`WorkspaceInterface`], read off
/// the UI thread (the workspace-sidebar task).
struct WorkspaceOpenParams {
    workspace: String,
    worktree: String,
    editor_name: String,
    /// The in-progress task on the workspace, when resolvable.
    task: Option<Box<shelbi_state::IssueFile>>,
}

/// A message from a workspace-open background job (all run off the UI thread).
enum WorkspaceJobMsg {
    /// A workspace-open resolve finished: build the interface or show the error.
    Opened(Result<WorkspaceOpenParams, String>),
    /// A content-session ensure finished: bind the role or fall back to the
    /// agent view with a status note.
    ContentReady(shelbi_app::ReviewRole, Result<(), String>),
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

/// The blocking startup work the shell runs off its UI thread so the first frame
/// draws without waiting on it (`rt-tui-headless-startup-block`): starting the
/// on-demand hub daemon and bringing up the orchestrator dashboard session.
/// Behind a trait so the shell's "first frame before a slow daemon" guarantee is
/// testable with a stubbed (slow) bootstrap.
trait Bootstrap: Send + Sync {
    /// Start the daemon (if not already running) and bring up `project`'s
    /// orchestrator dashboard. Blocking; returns a user-facing message on
    /// failure.
    fn bootstrap(&self, project: &str) -> Result<(), String>;
}

/// The production [`Bootstrap`]: start the hub daemon, then bootstrap the
/// orchestrator session. These used to run synchronously in `run_main` before
/// the shell started — the `ensure_daemon_running` socket wait (up to 10 s on a
/// cold/slow daemon) and the cold `ensure_dashboard` agent launch were the bulk
/// of the headless startup block, drawn nothing until they finished.
struct DaemonBootstrap;

impl Bootstrap for DaemonBootstrap {
    fn bootstrap(&self, project: &str) -> Result<(), String> {
        // A daemon that won't start shouldn't wedge the UI: surface it as a
        // status note, not a hard failure (read-only session viewing still
        // works, and the mutation/version gate reports a genuinely broken daemon
        // on first use). The dashboard bootstrap is the one that must succeed for
        // the orchestrator session to attach, so its error propagates.
        if let Err(e) = shelbi_state::ensure_daemon_running() {
            tracing::warn!(error = %e, "could not start the hub daemon");
        }
        shelbi_orchestrator::ensure_dashboard(project)
            .map(|_status| ())
            .map_err(|e| format!("couldn't bring up the orchestrator: {e}"))
    }
}

/// What the palette reads from disk besides the sidebar model: the other
/// registered projects (switch targets and the Projects column) and the edit
/// targets that exist. Behind a trait so a test can open the palette over a
/// fixed project list without touching the real Shelbi root.
trait PaletteInputs: Send + Sync {
    /// The registered projects other than `project`, as switch targets.
    fn other_projects(&self, project: &str) -> Vec<shelbi_app::command::ProjectItem>;
    /// The `edit:*` targets that exist on disk for `project`.
    fn edit_targets(&self, project: &str) -> Vec<shelbi_app::command::EditItem>;
    /// The palette's Projects column, `current` leading.
    fn projects_column(&self, current: &str) -> Vec<shelbi_app::overlay::palette::ProjectEntry>;
}

/// The production [`PaletteInputs`]: small local reads of the Shelbi root.
struct DiskPaletteInputs;

impl PaletteInputs for DiskPaletteInputs {
    fn other_projects(&self, project: &str) -> Vec<shelbi_app::command::ProjectItem> {
        shelbi_state::list_projects()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.name != project)
            .map(|p| shelbi_app::command::ProjectItem {
                label: p.display_label().to_string(),
                slug: p.name,
            })
            .collect()
    }
    fn edit_targets(&self, project: &str) -> Vec<shelbi_app::command::EditItem> {
        shelbi_app::command_model::edit_targets(project)
    }
    fn projects_column(&self, current: &str) -> Vec<shelbi_app::overlay::palette::ProjectEntry> {
        overlays::build_projects_column(current)
    }
}

/// The background workers bound to one project: the core snapshot refresher that
/// feeds the sidebar / board / machines views, the activity refresher that feeds
/// the activity feed, plus the two hub-socket subscribers (daemon change
/// notifications and layout events). All are created for one project at spawn and
/// never re-target themselves, so
/// [`ShellState::switch_project`] rebuilds the whole bundle for the new project.
/// Dropping the old bundle stops and joins every thread (each handle's `Drop`
/// does so) and, crucially, drops the old refresher's snapshot receiver — any
/// snapshot still in flight from the previous project is discarded rather than
/// rendered over the new project's data.
struct ProjectWorkers {
    refresher: refresh::Refresher<ShellSnapshot>,
    /// The activity feed's own refresher (sidebar/board never wait on its live
    /// `gh` title hydration — #1631). It is part of the bundle, and carries a
    /// **per-project** metadata cache, so a switch rebuilds it for the new
    /// project with a fresh cache: one project's resolved titles can't leak into
    /// another's feed.
    activity_refresher: refresh::Refresher<ActivityData>,
    /// The terminal `done`/`canceled` history's own refresher. It is the one
    /// board read that can hit the backend (a live GraphQL fetch when the
    /// done-history cache has aged out), so — like the activity refresher — it is
    /// split off the core snapshot: the open board, sidebar and machines paint
    /// from their fast local caches and the done column fills in when this lands.
    closed_refresher: refresh::Refresher<Option<ClosedPageRead>>,
    /// Wakes from the daemon change subscriber, drained each tick.
    change_rx: Receiver<shelbi_app::changes::ChangeWake>,
    /// Pushed layout events from the daemon poller, drained by
    /// [`ShellState::poll_layout_events`].
    layout_rx: Receiver<shelbi_state::LayoutEvent>,
    /// RAII handles for the two subscriber threads; held only so their `Drop`
    /// stops and joins them when the bundle is replaced. `None` in tests, which
    /// skip the real hub-socket subscribers.
    _changes: Option<shelbi_app::changes::ChangeSubscription>,
    _layout_sub: Option<shelbi_app::layout_sub::LayoutSubscription>,
}

/// Builds the per-project [`ProjectWorkers`] for a project. Injected onto
/// [`ShellState`] so a test can record which project each (re)build was asked for
/// and substitute a reader that touches no disk / socket; production uses
/// [`production_workers`].
type WorkersFactory = Arc<dyn Fn(&str) -> ProjectWorkers + Send + Sync>;

/// The production [`WorkersFactory`]. Each project's activity-title cache lives
/// in the factory, keyed by project, so it outlives the worker bundle a switch
/// retires: switching back reuses the titles already resolved (no re-hydration),
/// and a retired bundle's still-running hydration pass and the new bundle's
/// first pass for the same project serialize on the one cache instead of both
/// fetching the same ids.
fn production_workers_factory() -> WorkersFactory {
    let caches: Mutex<HashMap<String, Arc<Mutex<ActivityMetaCache>>>> = Mutex::new(HashMap::new());
    Arc::new(move |project: &str| {
        let cache = caches
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(project.to_string())
            .or_default()
            .clone();
        production_workers(project, cache)
    })
}

/// The production worker bundle for `project`: the snapshot and activity
/// refreshers reading real view data plus the two live hub-socket subscribers.
/// `activity_cache` is this project's own title cache (see
/// [`production_workers_factory`]).
fn production_workers(project: &str, activity_cache: Arc<Mutex<ActivityMetaCache>>) -> ProjectWorkers {
    let proj = project.to_string();
    let refresher = refresh::spawn(move |generation| read_snapshot(&proj, generation));
    // The activity feed resolves task titles, which on a remote (github) board is
    // live `gh` traffic that can take seconds on a large closed history. It runs
    // on its **own** refresher so a slow title-hydration pass never delays the
    // sidebar/board snapshot (#1631), and carries a persistent metadata cache so
    // resolution is incremental and terminates (closed-history titles resolve
    // once, then freeze) instead of re-sweeping done issues forever. The cache is
    // per project, so switching projects never carries one project's resolved
    // titles into another's feed.
    let proj_activity = project.to_string();
    let activity_refresher = refresh::spawn(move |_generation| {
        ActivityApp::read_activity_data(&proj_activity, &activity_cache)
    });
    // The terminal done/canceled history on its own refresher, so a stale-cache
    // GraphQL fetch never delays the open board's paint. Served from the
    // ten-minute done-history cache otherwise, so this adds no request volume.
    let proj_closed = project.to_string();
    let closed_refresher =
        refresh::spawn(move |_generation| KanbanApp::read_closed_page(&proj_closed));
    let (changes, change_rx) = shelbi_app::changes::spawn(project);
    let (layout_sub, layout_rx) = shelbi_app::layout_sub::spawn(project);
    ProjectWorkers {
        refresher,
        activity_refresher,
        closed_refresher,
        change_rx,
        layout_rx,
        _changes: Some(changes),
        _layout_sub: Some(layout_sub),
    }
}

/// Build a fresh set of the three embedded native-view apps for `project`, wired
/// exactly as the shell needs them (keymaps, the executor move-persister, the
/// machines reachability prober). Used at startup and rebuilt by
/// [`ShellState::switch_project`] so each app's `project_name` — which its card
/// moves, `load more`, and workspace-open actions target — follows the current
/// project rather than staying pinned to the launch one, and so a switch clears
/// the previous project's board/activity/machines data immediately.
fn build_embedded_apps(project: &str, keymaps: &Keymaps) -> (KanbanApp, ActivityApp, MachinesApp) {
    let mut kanban = KanbanApp::new(project);
    kanban.keymaps = keymaps.clone();
    // Board moves go through the shelbi-app executor (daemon-backed when
    // `dev.daemon_mutations` is on); the standalone process keeps its direct path.
    kanban.move_persister = Some(Arc::new(ExecutorMovePersister));
    let activity = ActivityApp::new(project);
    // The machines view probes remote reachability in the background (off the UI
    // thread and the refresh worker); the prober winds down when the app drops.
    let mut machines = MachinesApp::new(project);
    machines.enable_reachability();
    (kanban, activity, machines)
}

/// Drop `old` on a background thread instead of the UI thread. A project switch
/// replaces per-project state whose `Drop` *joins* threads — the refreshers wait
/// out an in-flight read (the activity feed's cold title hydration measured
/// ~20s on a large remote board), the hub subscribers wait out a 500ms read
/// slice, the machines prober an SSH probe — and joining any of them inline
/// froze the switch frame on the previous project. Retired state is already
/// unreachable from the UI (its receivers go with it), so nothing it still
/// produces can be rendered; it only winds down. If the thread can't be
/// spawned, the value drops inline (the old, blocking behavior).
fn retire<T: Send + 'static>(old: T) {
    let _ = std::thread::Builder::new()
        .name("shelbi-shell-retire".into())
        .spawn(move || drop(old));
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
    /// The off-thread startup work (daemon start + dashboard bootstrap), behind a
    /// seam so a slow bootstrap can be stubbed in tests
    /// (`rt-tui-headless-startup-block`).
    bootstrap: Arc<dyn Bootstrap>,
    /// The in-flight startup job and the project it bootstraps, drained by
    /// [`ShellState::poll_startup`]. `Some` from [`ShellState::spawn_startup`]
    /// until the daemon + dashboard bootstrap finishes; the first frame draws
    /// while it is still pending. A project switch replaces it (dropping the old
    /// project's receiver), so a late result for a project the user already left
    /// never lands in the new project's view or status.
    startup_rx: Option<(String, Receiver<Result<(), String>>)>,
    /// The open review interface (panel + content terminal view), when
    /// `main_view` is [`MainView::Review`]. `rt-tui-review`.
    review: Option<ReviewInterface>,
    /// The open workspace interface (panel + content terminal view), when
    /// `main_view` is [`MainView::Workspace`] (the workspace-sidebar task).
    workspace: Option<WorkspaceInterface>,
    /// In-flight workspace background work (open resolve, content ensure).
    /// Drained by [`ShellState::poll_workspace`].
    workspace_rx: Option<Receiver<WorkspaceJobMsg>>,
    /// The workspace an open-resolve is in flight for, so a resolve that lands
    /// after the user navigated away is dropped.
    workspace_opening: Option<String>,
    /// In-flight review background work (open resolution, content ensure,
    /// approve, reject). Drained by [`ShellState::poll_review`].
    review_rx: Option<Receiver<ReviewJobMsg>>,
    /// The daemon-facing review operations (resolve / load), behind a seam so the
    /// queued-load flow is testable with a stubbed daemon (`rt-tui-review-load-queued`).
    review_backend: Arc<dyn ReviewBackend>,
    /// The task a review-open resolve is in flight for, so a resolve that lands
    /// after the user navigated away (or started opening a different review) is
    /// dropped (`rt-tui-review-load-queued`).
    review_opening: Option<String>,
    /// The task this client loaded onto a slot and is now waiting to see serve:
    /// the matching daemon `ReviewOpened` opens the interface, while a
    /// background `ReviewOpened` (another client's load, a poller resume) for
    /// any other task never steals this client's view
    /// (`rt-tui-review-load-queued`).
    review_open_pending: Option<String>,
    /// The background workers for the current project (snapshot refresher + the
    /// change and layout hub-socket subscribers). `None` until
    /// [`ShellState::start_project_workers`] runs at startup; rebuilt for the new
    /// project by [`ShellState::switch_project`] so every native view follows the
    /// current project.
    workers: Option<ProjectWorkers>,
    /// Factory that builds [`ProjectWorkers`] for a project, injected so a test
    /// can record the project each build is asked for.
    make_workers: WorkersFactory,
    /// This process's own change bus, the setting-off fallback that feeds layout
    /// events when the producing poller lives in-process rather than the daemon
    /// (`rt-daemon-layout-split`). Unlike the hub-socket subscriber it is
    /// project-agnostic — [`ShellState::poll_layout_events`] filters it by the
    /// current project each tick — so a project switch needs no rebind.
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
    /// The last sidebar model each visited project had, keyed by project name,
    /// stashed when switching *away*. A switch *back* re-applies the cached model
    /// at once so the sidebar is populated on the next frame (a warm switch is
    /// visually immediate) instead of flashing the skeleton while the first
    /// refresh runs; the fresh read replaces it within ~one refresh. Only ever
    /// read for the key that matches the project it was stored under, so it can
    /// never show another project's data.
    sidebar_cache: HashMap<String, SidebarModel>,
    /// Config-only sidebar skeletons (label + declared workspace rows with a
    /// loading badge) for every registered project, built off the UI thread by
    /// [`ShellState::spawn_skeleton_prefetch`]. A *cold* switch (no
    /// `sidebar_cache` entry yet) paints the target's entry so its workspace rows
    /// show on the next frame without any IO on the switch path; like
    /// `sidebar_cache`, only ever read for its own key.
    skeletons: Arc<Mutex<HashMap<String, SidebarModel>>>,
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
    /// Platform convention for rendering chord hints in the sidebar footer —
    /// detected once at construction so per-frame rendering never re-probes.
    display_style: DisplayStyle,
    /// Launch-time sidebar status line (first-run hint / startup-warning count),
    /// claimed once at construction and stamped onto each refreshed sidebar
    /// model. Empty when there is nothing to surface.
    startup_status: String,
    main_view: MainView,
    /// Rects from the last draw, for mouse hit-testing.
    sidebar_rect: Rect,
    main_rect: Rect,
    /// `true` while the user is dragging the sidebar/main divider. The drag
    /// captures all mouse motion until the button is released, regardless of
    /// where the pointer travels.
    sidebar_dragging: bool,
    /// `true` while the pointer hovers the divider column. Drives the drag
    /// handle's hover highlight; only flipped (and repainted) when the pointer
    /// crosses the column, so idle motion elsewhere stays cheap.
    divider_hover: bool,
    /// The main-area size last reported to the live session.
    reported_main: Option<Size>,
    /// When `Some`, the user is typing a scrollback search query.
    search_input: Option<String>,
    /// Set when the shell should re-exec itself on exit (the daemon told it it
    /// is out of date, or a `shelbi reload` signalled a re-exec). Checked by
    /// [`run_with`] after the event loop ends.
    should_reexec: bool,
    /// This client's stable id for the attached-clients registry (generated once
    /// per process). The daemon-push listener registers under it, and "Detach
    /// Other Clients" detaches everyone *but* this id.
    client_id: String,
    /// How many *other* clients are attached right now (maintained by the
    /// background poller). Gates the "Detach Other Clients" palette command.
    other_clients: Arc<AtomicUsize>,
    /// Set (with a reason) when the daemon pushes a detach for this client
    /// (`shelbi detach` / another window's "Detach Other Clients"). The run loop
    /// observes it, quits, and prints the reason on exit.
    detach_signal: Arc<DetachSignal>,
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
    /// A file (or directory) to open in the user's editor. The run loop takes it
    /// after handling input: it hands the terminal to the editor and waits, then
    /// restores the shell (the TUI's half of the palette's `edit:*` commands).
    pending_editor: Option<PathBuf>,
    /// Where the palette's on-disk inputs come from (disk in production).
    palette_inputs: Arc<dyn PaletteInputs>,
    /// Test seam: when set, [`ShellState::dispatch_effect`] records each effect
    /// here instead of performing it, so a smoke test can assert what a UI path
    /// reached without running the effect.
    #[cfg(test)]
    effect_tap: Option<Vec<Effect>>,
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
        Self::new_with(
            project,
            connector,
            caps,
            Arc::new(DaemonLifecycle),
            Arc::new(DaemonBootstrap),
        )
    }

    fn new_with(
        project: &str,
        connector: Arc<dyn session::Connector>,
        caps: Caps,
        lifecycle: Arc<dyn ShellLifecycle>,
        bootstrap: Arc<dyn Bootstrap>,
    ) -> Self {
        let notice = caps.keyboard_notice().map(|text| Notice {
            text,
            until: Instant::now() + Duration::from_secs(NOTICE_SECS),
        });
        // Load the keymaps once: the palette's bindings and opener chord come
        // from here, and the embedded native-view handlers share them. A load
        // failure degrades to the embedded defaults; the sidebar surfaces the
        // diagnostic count.
        let (keymaps, diags) = load_keymaps(Some(project));
        let palette_chord = keymaps
            .global
            .first_chord_for(GlobalAction::OpenPalette)
            .copied();
        let display_style = DisplayStyle::detect();
        // Claim the one-time first-run hint, else surface any keymap-diagnostic
        // count. Mirrors the former sidebar's launch status line.
        let startup_status = sidebar::sidebar_startup_status_line(diags.len());
        let (kanban, activity, machines) = build_embedded_apps(project, &keymaps);
        // Seed the sidebar width from the user's saved choice (if any). The
        // stored value is the raw user choice; `draw` clamps it to the live
        // window at render time without overwriting the saved value. A read
        // error (or a never-dragged divider) leaves the built-in default.
        let client = {
            let mut c = ClientState::new(project);
            if let Ok(Some(w)) = shelbi_state::sidebar_width() {
                c.set_sidebar_width(w);
            }
            c
        };
        Self {
            client,
            sessions: SessionManager::new(project, connector.clone()).with_relauncher(
                std::sync::Arc::new(relaunch::PersistentRelauncher {
                    project: project.to_string(),
                }),
            ),
            connector,
            lifecycle,
            bootstrap,
            startup_rx: None,
            review: None,
            review_rx: None,
            review_backend: Arc::new(DaemonReviewBackend),
            review_opening: None,
            review_open_pending: None,
            workspace: None,
            workspace_rx: None,
            workspace_opening: None,
            workers: None,
            make_workers: production_workers_factory(),
            skeletons: Arc::new(Mutex::new(HashMap::new())),
            layout_bus: None,
            last_review_reconcile: Instant::now(),
            reconcile_rx: None,
            caps,
            sidebar_model: None,
            sidebar_cache: HashMap::new(),
            kanban,
            activity,
            machines,
            keymaps,
            display_style,
            startup_status,
            main_view: MainView::Session,
            sidebar_rect: Rect::default(),
            main_rect: Rect::default(),
            sidebar_dragging: false,
            divider_hover: false,
            reported_main: None,
            search_input: None,
            should_reexec: false,
            client_id: shelbi_client::fresh_client_id(),
            other_clients: Arc::new(AtomicUsize::new(0)),
            detach_signal: Arc::new(DetachSignal::default()),
            notice,
            overlay: None,
            palette_chord,
            status: None,
            job: None,
            pending_editor: None,
            palette_inputs: Arc::new(DiskPaletteInputs),
            #[cfg(test)]
            effect_tap: None,
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
        if let Some(mut sidebar) = snap.sidebar {
            // The launch status line is startup state (claimed once), not a
            // per-refresh board read — stamp it onto the freshly-built model.
            sidebar.status_line = self.startup_status.clone();
            // Keep the selection in range as rows come and go.
            let view = SidebarView::build(&sidebar);
            self.client.clamp_selection(view.selectable_count());
            // Firm up an open workspace panel's header from the fresh model (the
            // same source the sidebar badge reads) so a late in-progress
            // assignment — or a handoff that idles the workspace — reflects in
            // the header within a poll tick, instead of freezing at the value it
            // held when the view opened. Only the header status is re-derived;
            // the panel's task title / description and content sessions stay put.
            if let Some(ws) = self.workspace.as_mut() {
                let status = ws_status_from_model(&sidebar, ws.workspace());
                ws.set_header_status(status);
            }
            // An open review's server line follows its sidebar row, as the
            // desktop's Review view does.
            if let Some(review) = self.review.as_mut() {
                let row = sidebar.reviews.iter().find(|r| r.task_id == review.task_id());
                review.set_server(row);
            }
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
        let other_clients_attached = self.other_clients.load(Ordering::SeqCst) > 0;
        let model = shelbi_app::command_model::command_model_from(
            project,
            sidebar,
            other_clients_attached,
            self.palette_inputs.other_projects(project),
            self.palette_inputs.edit_targets(project),
        );
        CommandRegistry::new().entries(&model)
    }

    fn sidebar_view(&self) -> Option<SidebarView> {
        self.sidebar_model.as_ref().map(SidebarView::build)
    }

    /// The per-frame sidebar footer inputs (keybind hint + Zen glyph), owned so
    /// the draw closure holds no borrow of the rest of the shell state.
    fn sidebar_chrome(&self) -> sidebar::SidebarChrome {
        sidebar::SidebarChrome::from_keymaps(&self.keymaps, self.display_style)
    }

    fn selection(&self) -> usize {
        self.client.sidebar_selection()
    }

    fn sidebar_width(&self) -> u16 {
        self.client.sidebar_width()
    }

    /// Open a sidebar target and move focus to the main area: a session attaches
    /// in the main area, a native view swaps it, and a review builds its
    /// interface there — every one of them takes main focus, so opening a row
    /// (click, Space, or Enter) lands the user on what they opened regardless of
    /// its kind. (Plain selection moves — arrows / j / k — never call this, so
    /// they leave focus on the sidebar and the main view unchanged.) Records the
    /// target as the current project's remembered view (via
    /// [`ClientState::set_view`]) so a later
    /// [`switch_project`](Self::switch_project) back to this project restores it
    /// — the per-client last-view-per-project memory the plan wants.
    fn show(&mut self, target: RowTarget) {
        // Opening any row — a session, a native view, a review, or a workspace
        // interface — moves keyboard focus to the main area immediately, up
        // front, before any off-thread open work. Owning the focus move here —
        // the one function every open path routes through (click, Enter/Space,
        // the palette) — rather than in each match arm or only when an async
        // open finally completes keeps focus and the focused-pane styling
        // uniform: a review / workspace open that is slow, fails to resolve, or
        // is already open never strands focus on the sidebar while the user
        // waits (the async builders below re-assert it on success, but the
        // failure and already-open paths never reach them). A machine group
        // header only toggles its collapse state — it is not an open — so it
        // leaves focus where it is.
        if !matches!(target, RowTarget::Machine(_)) {
            self.client.focus_main();
        }
        // A dev workspace that has a running task opens the *workspace sidebar* (a
        // transient interface like review, built off-thread) rather than
        // attaching its agent session in the main area under the nav sidebar. An
        // idle workspace keeps the nav sidebar and shows the idle placeholder (the
        // `RowTarget::Session` path below). Diverted before the view is recorded,
        // so a later Back restores the view that was active before the workspace
        // opened — the interface itself is never remembered.
        if let RowTarget::Session(SessionRef::Workspace(name)) = &target {
            if self.workspace_has_task(name) {
                let name = name.clone();
                self.begin_workspace(name);
                self.dirty = true;
                return;
            }
        }
        // Record the view first (a Review target has no `View` and is transient,
        // so it is deliberately not remembered).
        if let Some(view) = row_target_to_view(&target) {
            self.client.set_view(view);
        }
        match target {
            RowTarget::Session(r) => {
                // Leaving an open review / workspace for a session: drop the
                // interface so it stops rendering over the main area (it draws at
                // the frame level regardless of `main_view`). Dropping detaches
                // the content views; the daemon keeps the sessions, so reopening
                // reattaches (`rt-tui-idle-workspace-open`).
                self.review = None;
                self.workspace = None;
                self.main_view = MainView::Session;
                self.reported_main = None; // force a resize report for the new session
                // Record the view (orchestrator chat or a workspace session).
                let view = match &r {
                    SessionRef::Orchestrator => View::Session("orch".into()),
                    SessionRef::Workspace(w) => View::Session(w.clone()),
                    // Content sessions live inside the review / workspace
                    // interface's own manager, never the main one; these arms
                    // only keep the match exhaustive.
                    SessionRef::Review { slot, role } => {
                        View::Session(format!("review/{slot}/{role}"))
                    }
                    SessionRef::WorkspaceContent { workspace, role } => {
                        View::Session(format!("ws/{workspace}/{role}"))
                    }
                };
                self.client.set_view(view);
                self.sessions.show(r);
                // Focus moved to the main area up front, in `show`.
            }
            RowTarget::Native(v) => {
                // As above: a native view also replaces the main area, so drop
                // any open review / workspace interface drawn over it.
                self.review = None;
                self.workspace = None;
                self.client.set_view(v.clone());
                self.main_view = MainView::Native(v);
                // Focus moved to the main area up front, in `show` — so the
                // keyboard drives the board / feed / machines list immediately,
                // the same as opening a session.
            }
            RowTarget::Review(id) => {
                // Review has no `View` variant (it is a transient interface, not a
                // remembered main view), so it is not recorded.
                self.begin_review(id);
            }
            RowTarget::Machine(name) => {
                // A machine group header toggles its collapse state (persisted to
                // `state.json`). Mirror the new state into the cached model so the
                // next render reflects it without waiting for a refresh tick; a
                // disk failure leaves the row reading what's on disk.
                match shelbi_state::toggle_sidebar_machine_collapsed(&name) {
                    Ok(now_collapsed) => {
                        if let Some(m) = self.sidebar_model.as_mut() {
                            if now_collapsed {
                                m.collapsed_machines.insert(name);
                            } else {
                                m.collapsed_machines.remove(&name);
                            }
                        }
                    }
                    Err(e) => self.status = Some(format!("collapse failed: {e}")),
                }
            }
        }
        self.dirty = true;
    }

    /// Switch this client to another open project, restoring the view it last
    /// had there (its default view the first time). The session manager is
    /// rebuilt for the new project and the restored view is applied to the main
    /// area. Agents in the old project keep running — switching is a view
    /// change, not a quit. Invoked from the command palette (`dispatch_effect`).
    /// Build (or rebuild) the background workers for the current project and ask
    /// the fresh refresher for a snapshot right away, so the new project's views
    /// render without waiting for the periodic tick. The previous bundle (if any)
    /// is [retired](retire): its threads wind down off the UI thread, and its
    /// receivers go with it, so any in-flight read from the old project is
    /// discarded rather than rendered.
    fn start_project_workers(&mut self) {
        let project = self.client.project().unwrap_or_default().to_string();
        let make = self.make_workers.clone();
        if let Some(old) = self.workers.replace(make(&project)) {
            retire(old);
        }
        self.request_refresh();
    }

    /// Build every registered project's config-only sidebar skeleton off the UI
    /// thread (see [`ShellState::skeletons`]), replacing the set in one swap when
    /// done. Run once after the first frame; a project added later just falls
    /// back to the bare skeleton on its first switch.
    fn spawn_skeleton_prefetch(&self) {
        let skeletons = self.skeletons.clone();
        let _ = std::thread::Builder::new()
            .name("shelbi-shell-skeletons".into())
            .spawn(move || {
                let built: HashMap<String, SidebarModel> = shelbi_state::list_projects()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|p| {
                        shelbi_orchestrator::sidebar_model::read_config_skeleton(&p.name)
                            .map(|m| (p.name, m))
                    })
                    .collect();
                *skeletons.lock().unwrap_or_else(|p| p.into_inner()) = built;
            });
    }

    /// The sidebar model a switch into `project` paints on its first frame, from
    /// memory only: the model it had when last left (warm — populated), else its
    /// prefetched config skeleton (cold — label + workspace rows with a loading
    /// badge), else the bare skeleton (nav rows + "Loading…"). Each source is
    /// read only for `project`'s own key.
    fn switch_frame_sidebar(&self, project: &str) -> SidebarModel {
        if let Some(cached) = self.sidebar_cache.get(project) {
            return cached.clone();
        }
        // `try_lock`: the prefetch holds the lock only for its final swap, but
        // the switch frame must never wait on it.
        let prefetched = self
            .skeletons
            .try_lock()
            .ok()
            .and_then(|m| m.get(project).cloned());
        prefetched.unwrap_or_else(|| SidebarModel::skeleton(project))
    }

    /// Ask the current project's refreshers (the core snapshot and the activity
    /// feed) for fresh data. No-op before the workers are started.
    fn request_refresh(&self) {
        if let Some(w) = self.workers.as_ref() {
            w.refresher.request();
            w.activity_refresher.request();
            w.closed_refresher.request();
        }
    }

    /// Drain the current project's change subscriber, returning whether any change
    /// arrived and the names of the workspaces whose change carried one (so the
    /// shown-workspace idle attach can be retried). No-op before the workers are
    /// started.
    fn drain_change_wakes(&self) -> (bool, Vec<String>) {
        let mut changed = false;
        let mut woke = Vec::new();
        if let Some(w) = self.workers.as_ref() {
            while let Ok(wake) = w.change_rx.try_recv() {
                changed = true;
                if let shelbi_app::changes::ChangeWake::Workspace(ws) = wake {
                    woke.push(ws);
                }
            }
        }
        (changed, woke)
    }

    /// The freshest snapshot from the current project's refresher, or `None`. A
    /// snapshot still queued from a just-replaced (old-project) refresher went out
    /// with that refresher's receiver, so this only ever yields the current
    /// project's data.
    fn latest_snapshot(&self) -> Option<ShellSnapshot> {
        self.workers.as_ref().and_then(|w| w.refresher.latest())
    }

    /// The freshest activity read from the current project's activity refresher,
    /// or `None`. Like [`Self::latest_snapshot`], a read still queued from a
    /// just-replaced (old-project) refresher went out with that refresher's
    /// receiver, so this only ever yields the current project's activity data.
    fn latest_activity(&self) -> Option<ActivityData> {
        self.workers
            .as_ref()
            .and_then(|w| w.activity_refresher.latest())
    }

    /// The freshest terminal `done`/`canceled` page from the current project's
    /// closed refresher, or `None`. Like [`Self::latest_snapshot`], a read still
    /// queued from a just-replaced (old-project) refresher went out with that
    /// refresher's receiver, so this only ever yields the current project's data.
    /// The outer `Option` is "a read landed"; the inner is the read's own result
    /// (`None` when the store couldn't be built or the fetch failed).
    fn latest_closed(&self) -> Option<Option<ClosedPageRead>> {
        self.workers.as_ref().and_then(|w| w.closed_refresher.latest())
    }

    fn switch_project(&mut self, project: &str) {
        // Switching to the already-current project (e.g. Enter on the current
        // row in the palette's Projects column) is a no-op; skip the session
        // rebuild so it doesn't needlessly detach and reconnect.
        if self.client.project() == Some(project) {
            return;
        }
        // Stash the project we're leaving so switching back is a warm, immediate
        // paint rather than a skeleton flash (keyed by name, applied only for its
        // own key below).
        let leaving = self.client.project().map(str::to_string);
        if let (Some(current), Some(model)) = (leaving, self.sidebar_model.clone()) {
            self.sidebar_cache.insert(current, model);
        }
        // `ClientState::switch_project` records the current project's view and
        // restores the new project's remembered (or default) view.
        self.client.switch_project(project);
        // Nothing below may block: the next frame must show the target project.
        // Every piece of the old project's state whose `Drop` joins a thread (the
        // worker bundle, the embedded apps' reachability prober, the sessions'
        // connections) is swapped out and retired off the UI thread in one batch.
        //
        // Rebuild the session manager for the new project (the old project's
        // connection detaches, never kills).
        let old_sessions = std::mem::replace(
            &mut self.sessions,
            SessionManager::new(project, self.connector.clone()).with_relauncher(
                std::sync::Arc::new(relaunch::PersistentRelauncher {
                    project: project.to_string(),
                }),
            ),
        );
        // Rebuild the embedded native-view apps for the new project. This resets
        // their `project_name` — which card moves, `load more` and workspace-open
        // actions target, otherwise still the launch project after a switch — and
        // clears the previous project's board / activity / machines data so it is
        // never shown (the fresh reads refill them). Interaction state (selection,
        // scroll, in-flight optimistic moves) resetting is correct: it belonged to
        // the project we left.
        let (mut kanban, activity, machines) = build_embedded_apps(project, &self.keymaps);
        // Show the board's own "Loading…" overlay (not bare empty columns) until
        // the first off-thread read for the new project lands.
        kanban.board_loading = true;
        let old_apps = (
            std::mem::replace(&mut self.kanban, kanban),
            std::mem::replace(&mut self.activity, activity),
            std::mem::replace(&mut self.machines, machines),
        );
        // An open review / workspace interface belongs to the project we left: it
        // renders over the sidebar column and main area regardless of
        // `main_view`, so drop it (detaching its content view) along with any
        // open/load still in flight for it, whose late result must not land in
        // the new project.
        let old_interfaces = (self.review.take(), self.workspace.take());
        self.review_rx = None;
        self.review_opening = None;
        self.review_open_pending = None;
        self.reconcile_rx = None;
        self.workspace_rx = None;
        self.workspace_opening = None;
        retire((old_sessions, old_apps, old_interfaces));
        // Paint the target project's sidebar chrome at once, from memory only
        // (cached model, else prefetched config skeleton, else bare skeleton).
        // Either way the previous project's rows are gone this frame, and the
        // fresh off-thread read below replaces it within ~one refresh.
        self.sidebar_model = Some(self.switch_frame_sidebar(project));
        // Rebuild the background workers (snapshot + activity + closed refreshers,
        // change / layout subscribers) for the new project, retiring the old
        // project's bundle off the UI thread. Without this the sidebar, board and review interface
        // keep reading the launch project. `layout_bus` needs no rebind (it is
        // project-agnostic and filtered per tick); the daemon-push client
        // registration keeps its launch-project tag, which is informational only
        // (re-exec and detach notices don't depend on the active project).
        self.start_project_workers();
        self.reported_main = None;
        // Apply the restored view to the main area.
        let restored = self.client.view().clone();
        self.apply_restored_view(restored);
        // Bring up the new project's orchestrator off the UI thread, exactly as a
        // fresh launch in it would: without this only the launch project's
        // orchestrator was ever started, so a project whose orchestrator crashed
        // (or was never opened this run) showed an empty orchestrator view.
        // `ensure_dashboard` is idempotent (a live session is left alone, and the
        // per-project dashboard lock serializes racing callers), so switching
        // into a running project or back and forth never double-launches. This
        // replaces any still-pending startup job, dropping its receiver.
        self.status = Some(STARTUP_STATUS.to_string());
        self.spawn_startup();
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
            // `view_to_row_target` never yields a machine-toggle target (it has
            // no `View`), so this arm is only for exhaustiveness.
            RowTarget::Machine(_) => {}
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

    /// Detach every *other* client attached to this session, leaving this window
    /// running (the palette's "Detach Other Clients"). A best-effort control-socket
    /// round-trip; the outcome lands in the status line.
    fn detach_other_clients(&mut self) {
        let reason = format!("Detached by another window on {}.", shelbi_client::hostname());
        let selector = shelbi_proto::control::DetachSelector::AllExcept {
            client_id: self.client_id.clone(),
        };
        self.status = Some(match connect_control() {
            Some(mut client) => match client.detach_clients(selector, &reason) {
                Ok(0) => "No other windows are attached.".to_string(),
                Ok(n) => format!("Detached {n} other window{}.", if n == 1 { "" } else { "s" }),
                Err(e) => format!("Could not detach other windows: {e}"),
            },
            None => "Could not reach the daemon to detach other windows.".to_string(),
        });
        self.dirty = true;
    }

    fn activate_selection(&mut self) {
        if let Some(view) = self.sidebar_view() {
            if let Some(target) = view.target_at(self.selection()) {
                self.show(target);
            }
        }
    }

    /// Retry the main-area attach when one of `changed_workspaces` is the
    /// workspace currently shown and its session was idle/failed — so a session
    /// that just started attaches without the user re-selecting the row
    /// (`rt-tui-idle-workspace-open`). Returns whether a retry was kicked.
    fn retry_shown_workspace(&mut self, changed_workspaces: &[String]) -> bool {
        if !matches!(self.main_view, MainView::Session) {
            return false;
        }
        let shown = match self.sessions.current_target() {
            Some(SessionRef::Workspace(name)) => name.clone(),
            _ => return false,
        };
        if changed_workspaces.contains(&shown) {
            self.sessions.retry_if_stale()
        } else {
            false
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
                    // Overlays center on the full window (sidebar + main), so
                    // hit-test against that same rect — clicks land on the right
                    // row even where the overlay covers the sidebar.
                    let area = self.full_area();
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
            Event::Paste(s) => self.deliver_paste(&s),
            Event::FocusGained => self.forward_focus(true),
            Event::FocusLost => self.forward_focus(false),
            Event::Resize(_, _) => self.dirty = true,
        }
    }

    fn focus_is_main(&self) -> bool {
        self.client.focus() == Focus::Main
    }

    /// Deliver a bracketed paste to whatever session the main area currently
    /// shows — the orchestrator / workspace agent for a plain session, or the
    /// content session (review agent, workspace agent, diff, editor) owned by an
    /// open review or workspace interface — routing through that session's own
    /// manager so the inner program's bracketed-paste encoding is preserved.
    ///
    /// A terminal reports a drag-and-drop of a file as a bracketed paste of the
    /// dropped path, and Claude Code attaches an image from such a path. A drop
    /// targets the window, not whatever list holds keyboard focus, so it must
    /// reach the main-area session even when the nav sidebar or an interface
    /// panel is focused; focus follows the paste to the main area so the user
    /// can keep typing there afterward. Native board/activity/machines views
    /// have no terminal session, so a drop over one is a no-op.
    fn deliver_paste(&mut self, text: &str) {
        match self.main_view {
            MainView::Session => {
                self.client.focus_main();
                self.sessions.send_paste(text);
                self.dirty = true;
            }
            MainView::Review(_) => {
                self.client.focus_main();
                if let Some(r) = self.review.as_mut() {
                    r.paste_into_content(text);
                }
                self.dirty = true;
            }
            MainView::Workspace(_) => {
                self.client.focus_main();
                if let Some(w) = self.workspace.as_mut() {
                    w.paste_into_content(text);
                }
                self.dirty = true;
            }
            MainView::Native(_) => {}
        }
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
        // Vim-style focus moves (`Ctrl+H` / `Ctrl+L` by default) are global:
        // they fire from a focused terminal session the same as from the
        // sidebar, so they're intercepted here before focus routing and the
        // key is never forwarded to the session. Routing them through the
        // keymap means they only match the unambiguous `Char('h')`/`Char('l')`
        // + CONTROL form — on a terminal without keyboard-enhancement, Ctrl+H
        // arrives as `Backspace` and so can't be confused for this action
        // (`chord_from_event` maps it to `Key::Backspace`, which `ctrl-h`
        // never matches), leaving plain Backspace to reach the session.
        if let Some(action) = self.focus_move_action(&k) {
            self.move_focus(action);
            self.dirty = true;
            return;
        }
        if self.focus_is_main() {
            self.handle_main_key(k);
        } else {
            self.handle_sidebar_key(k);
        }
    }

    /// Resolve `k` to a focus-move action (`FocusSidebar` / `FocusMain`) via
    /// the global keymap, or `None` for any other key. The keymap lookup is
    /// what keeps Ctrl+H distinct from Backspace (see [`ShellState::handle_key`]).
    fn focus_move_action(&self, k: &KeyEvent) -> Option<GlobalAction> {
        let chord = crate::keymap::chord_from_event(*k)?;
        match self.keymaps.global.dispatch(chord) {
            Some(a @ (GlobalAction::FocusSidebar | GlobalAction::FocusMain)) => Some(a),
            _ => None,
        }
    }

    /// Carry out a focus-move action. With a review open the review interface
    /// owns the main area, so the moves step between its panel (left) and its
    /// content view (right); otherwise they step between the nav sidebar
    /// (left) and the main pane (right). Moving to the pane that already has
    /// focus is a harmless no-op.
    fn move_focus(&mut self, action: GlobalAction) {
        if matches!(self.main_view, MainView::Review(_)) {
            // The review always owns the main area while open (opening it
            // focuses main); keep that invariant, then move within it.
            self.client.focus_main();
            if let Some(r) = self.review.as_mut() {
                match action {
                    GlobalAction::FocusSidebar => r.focus_panel(),
                    GlobalAction::FocusMain => r.focus_content(),
                    _ => {}
                }
            }
            return;
        }
        if matches!(self.main_view, MainView::Workspace(_)) {
            // Same invariant as review: the workspace interface owns the main
            // area, so the focus moves step between its panel and content view.
            self.client.focus_main();
            if let Some(w) = self.workspace.as_mut() {
                match action {
                    GlobalAction::FocusSidebar => w.focus_panel(),
                    GlobalAction::FocusMain => w.focus_content(),
                    _ => {}
                }
            }
            return;
        }
        match action {
            GlobalAction::FocusSidebar => self.client.focus_sidebar(),
            GlobalAction::FocusMain => self.client.focus_main(),
            _ => {}
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
        let entries = self.palette_entries_from_model();
        // The Projects column lists every registered project, the current one
        // leading with the filled disc; Enter on a row switches to it.
        let projects = self
            .client
            .project()
            .map(|p| self.palette_inputs.projects_column(p))
            .unwrap_or_default();
        self.overlay = Some(ActiveOverlay::palette(entries, projects));
    }

    /// The bare "Switch Project": the palette with its Projects column focused,
    /// the TUI's project switcher.
    fn open_project_switcher(&mut self) {
        self.open_palette();
        if let Some(ActiveOverlay::Palette(p)) = &mut self.overlay {
            p.focus_projects();
        }
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
                // Load the queued task onto the chosen free slot through the
                // daemon, off the UI thread; the daemon's `ReviewOpened` event
                // opens the interface once the slot is serving.
                self.start_review_load(task_id, slot);
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

    /// Fold the real terminal capabilities in once the post-first-frame kitty
    /// probe has run, arming the one-time keyboard-protocol notice if warranted.
    /// Construction starts from [`Caps::detect_fast`] (no notice), so this is
    /// where the notice is first armed (`rt-tui-headless-startup-block`).
    fn set_caps(&mut self, caps: Caps) {
        self.caps = caps;
        self.notice = caps.keyboard_notice().map(|text| Notice {
            text,
            until: Instant::now() + Duration::from_secs(NOTICE_SECS),
        });
        self.dirty = true;
    }

    /// Spawn the off-thread startup job (daemon start + dashboard bootstrap). The
    /// event loop draws its first frame and stays interactive while this runs;
    /// [`ShellState::poll_startup`] folds the result in (`rt-tui-headless-startup-block`).
    fn spawn_startup(&mut self) {
        let Some(project) = self.client.project().map(str::to_string) else {
            return;
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let bootstrap = self.bootstrap.clone();
        let job_project = project.clone();
        if std::thread::Builder::new()
            .name("shelbi-shell-startup".into())
            .spawn(move || {
                let _ = tx.send(bootstrap.bootstrap(&job_project));
            })
            .is_ok()
        {
            self.startup_rx = Some((project, rx));
        }
    }

    /// Poll the off-thread startup job without blocking. On success, re-attach
    /// the orchestrator/workspace session now that the dashboard is up (an attach
    /// that failed before bootstrap retries); on failure, surface the message.
    /// Returns `true` when the job finished (so the caller redraws).
    fn poll_startup(&mut self) -> bool {
        let result = match &self.startup_rx {
            // A job for a project the user has since left is stale: drop it
            // without touching the current project's view or status. (A switch
            // already replaces the job; this guards any other path.)
            Some((project, _)) if self.client.project() != Some(project.as_str()) => {
                self.startup_rx = None;
                return false;
            }
            Some((_, rx)) => match rx.try_recv() {
                Ok(r) => r,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => {
                    Err("startup stopped unexpectedly".to_string())
                }
            },
            None => return false,
        };
        self.startup_rx = None;
        match result {
            Ok(()) => {
                // The orchestrator session exists now; re-attach if the main area
                // is showing a session (a native view doesn't need it).
                if matches!(self.main_view, MainView::Session) {
                    self.sessions.reconnect();
                }
                if self.status.as_deref() == Some(STARTUP_STATUS) {
                    self.status = None;
                }
            }
            Err(msg) => self.status = Some(msg),
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
    /// directly; the error log, the add-project form, the reject prompt and the
    /// Zen intro open as overlays; an `edit:*` target opens in the user's
    /// editor; mutations run off the UI thread. Every effect and every
    /// mutation has its own arm (no catch-all), so a new one fails to compile
    /// until the TUI handles it.
    fn dispatch_effect(&mut self, effect: Effect) {
        #[cfg(test)]
        if let Some(tap) = self.effect_tap.as_mut() {
            tap.push(effect);
            return;
        }
        match effect {
            Effect::ShowView(v) => self.show_view_effect(v),
            Effect::FocusWorkspace { workspace, .. } => {
                self.show(RowTarget::Session(SessionRef::Workspace(workspace)))
            }
            Effect::LoadReview { task_id, .. } => self.begin_review(task_id),
            Effect::OpenErrorLog { project } => {
                self.overlay = Some(ActiveOverlay::error_log(project));
            }
            Effect::OpenEditor { target } => self.open_editor(&target),
            // A bare switch (the palette's "Switch Project") picks the target in
            // the Projects column; a named one switches.
            Effect::SwitchProject { project } if project.is_empty() => self.open_project_switcher(),
            Effect::SwitchProject { project } => self.switch_project(&project),
            Effect::AddProject => {
                // Open the in-process add-project form over the main area. On
                // submit the shell validates + scaffolds through the shared
                // `project_create` engine (the same path `shelbi init` uses),
                // off the UI thread, then switches to the new project.
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                self.overlay = Some(ActiveOverlay::add_project(&cwd));
            }
            // The registry lists New Window only for a multi-window client
            // (`CommandModel::multi_window`, the desktop app); this terminal
            // is one window, so say where another one comes from.
            Effect::NewWindow { .. } => {
                self.status = Some(
                    "New Window is a desktop command: run shelbi in another terminal".to_string(),
                )
            }
            Effect::Detach => self.quit(QuitAction::CloseUi),
            Effect::DetachOtherClients => self.detach_other_clients(),
            Effect::QuitProject { .. } => self.quit(QuitAction::QuitProject),
            Effect::QuitShelbi => self.quit(QuitAction::QuitShelbi),
            Effect::Mutate(Mutation::ToggleZen { project }) => self.toggle_zen(project),
            // The palette's review decisions. Approving the review that is open
            // here goes through its panel (so the panel holds while it merges);
            // any other runs off the UI thread with a status line. Reject asks
            // for the reason first unless one came with it.
            Effect::Mutate(Mutation::ApproveReview { project, id }) => {
                if self.review.as_ref().map(|r| r.task_id()) == Some(id.as_str()) {
                    self.apply_review_action(ReviewAction::Approve);
                } else {
                    let m = Mutation::ApproveReview {
                        project,
                        id: id.clone(),
                    };
                    self.spawn_job(move || match run_review_mutation(&m) {
                        Ok(()) => format!("approved {id}"),
                        Err(e) => format!("approve failed: {e}"),
                    });
                }
            }
            Effect::Mutate(Mutation::RejectReview { id, reason, .. }) if reason.trim().is_empty() => {
                self.overlay = Some(ActiveOverlay::reject_reason(id));
            }
            Effect::Mutate(m @ Mutation::RejectReview { .. }) => {
                self.spawn_mutation(m, "rejected", "reject failed")
            }
            // The form's submit, wherever it came from: validate, scaffold, switch.
            Effect::Mutate(Mutation::AddProject { name, root, mode }) => {
                self.submit_add_project(name, root, mode)
            }
            // The board's card mutations. The embedded board sends its own
            // through its move persister; one arriving here (no palette entry
            // produces them) goes straight to the daemon.
            Effect::Mutate(m @ Mutation::MoveIssue { .. }) => self.spawn_mutation(m, "moved", "move failed"),
            Effect::Mutate(m @ Mutation::StartIssue { .. }) => {
                self.spawn_mutation(m, "started", "start failed")
            }
            Effect::Mutate(m @ Mutation::AssignIssue { .. }) => {
                self.spawn_mutation(m, "assigned", "assign failed")
            }
            Effect::Mutate(m @ Mutation::EditIssue { .. }) => self.spawn_mutation(m, "edited", "edit failed"),
            Effect::Mutate(m @ Mutation::AddIssue { .. }) => self.spawn_mutation(m, "added", "add failed"),
            Effect::Mutate(m @ Mutation::PrioritizeIssue { .. }) => {
                self.spawn_mutation(m, "reordered", "reorder failed")
            }
        }
    }

    /// Send `m` to the daemon off the UI thread; the status line reports `done`
    /// or `failed: <why>`.
    fn spawn_mutation(&mut self, m: Mutation, done: &'static str, failed: &'static str) {
        self.spawn_job(move || match run_review_mutation(&m) {
            Ok(()) => done.to_string(),
            Err(e) => format!("{failed}: {e}"),
        });
    }

    /// Open an `edit:*` target in the user's editor: the run loop hands the
    /// terminal over on its next turn ([`run_editor`]).
    fn open_editor(&mut self, target: &shelbi_app::EditTarget) {
        let Some(project) = self.client.project() else {
            return;
        };
        match shelbi_app::command_model::edit_target_path(project, target) {
            Some(path) => self.pending_editor = Some(path),
            None => self.status = Some("couldn't find that file to edit".to_string()),
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

    // --- review interface (rt-tui-review / rt-tui-review-load-queued) -------

    /// Begin opening the review for `task_id`. First resolves off the UI thread
    /// whether the task is already serving on a slot or still queued
    /// (`rt-tui-review-load-queued`); [`ShellState::on_review_resolved`] then
    /// either builds the interface or raises the slot picker. The main view is
    /// left alone until that lands, so a queued task raises the picker rather
    /// than flashing an empty review.
    fn begin_review(&mut self, task_id: String) {
        // Already showing this review — no-op.
        if self.review.as_ref().map(|r| r.task_id()) == Some(task_id.as_str())
            && matches!(self.main_view, MainView::Review(_))
        {
            return;
        }
        let Some(project) = self.client.project().map(str::to_string) else {
            return;
        };
        self.review_opening = Some(task_id.clone());
        self.status = Some(format!("opening review {task_id}…"));
        let (tx, rx) = std::sync::mpsc::channel();
        self.review_rx = Some(rx);
        let backend = self.review_backend.clone();
        std::thread::Builder::new()
            .name("shelbi-review-open".into())
            .spawn(move || {
                let _ = tx.send(ReviewJobMsg::Resolved(backend.resolve(&project, &task_id)));
            })
            .ok();
    }

    /// Act on a finished review-open resolve: build the interface (serving),
    /// raise the slot picker (queued with free slots), report that every slot is
    /// busy (queued with none free), or surface the error. A resolve for a task
    /// the user is no longer opening is dropped (`rt-tui-review-load-queued`).
    fn on_review_resolved(&mut self, result: Result<ResolvedReview, String>) {
        let Some(task) = self.review_opening.take() else {
            return; // navigated away / superseded — the resolve is stale
        };
        match result {
            Ok(ResolvedReview::Serving(p)) => self.build_review_interface(p),
            Ok(ResolvedReview::Queued {
                title,
                free_slots,
                total_slots,
            }) => self.open_review_picker(task, title, free_slots, total_slots),
            Err(e) => {
                self.review_open_pending = None;
                self.status = Some(format!("review failed: {e}"));
            }
        }
    }

    /// Build the native review interface from resolved params and switch the main
    /// area to it (`rt-tui-review`).
    fn build_review_interface(&mut self, p: ReviewOpenParams) {
        let project = self
            .client
            .project()
            .map(str::to_string)
            .unwrap_or_default();
        self.main_view = MainView::Review(p.task_id.clone());
        self.client.focus_main();
        self.review = Some(ReviewInterface::new(
            &project,
            self.connector.clone(),
            p.task_id,
            p.slot,
            p.worktree,
            p.editor_name,
            p.has_review_url,
            p.task.map(|b| *b),
        ));
        self.reported_main = None; // re-report size for the content view
        self.status = None;
        self.review_open_pending = None;
        self.dirty = true;
    }

    /// Raise the "Load for review" picker for a queued `task` over its `free`
    /// slots, or — when none are free — the appropriate no-slots report (every
    /// slot busy, or no review workspace configured at all). Loads nothing until
    /// the user confirms a slot (`rt-tui-review-load-queued`).
    fn open_review_picker(
        &mut self,
        task: String,
        title: String,
        free: Vec<crate::overlay::review_confirm::Slot>,
        total_slots: usize,
    ) {
        self.status = None;
        self.overlay = Some(if free.is_empty() {
            if total_slots == 0 {
                // No `review`-tagged workspace exists — the overlay's default
                // "no review workspace is configured" report.
                ActiveOverlay::review_confirm(task, title, Vec::new())
            } else {
                ActiveOverlay::review_busy_report(
                    task,
                    title,
                    "Every review slot is busy — free one to load this task.",
                )
            }
        } else {
            ActiveOverlay::review_confirm(task, title, free)
        });
        self.client.focus_main();
        self.dirty = true;
    }

    /// Load a queued `task_id` onto the free review `workspace` the picker chose,
    /// off the UI thread (`rt-tui-review-load-queued`). Records the task as
    /// pending so the daemon's matching `ReviewOpened` opens the interface.
    fn start_review_load(&mut self, task_id: String, workspace: String) {
        let Some(project) = self.client.project().map(str::to_string) else {
            return;
        };
        self.review_open_pending = Some(task_id.clone());
        self.status = Some(format!("loading {task_id} onto {workspace}…"));
        let (tx, rx) = std::sync::mpsc::channel();
        self.review_rx = Some(rx);
        let backend = self.review_backend.clone();
        let task = task_id.clone();
        std::thread::Builder::new()
            .name("shelbi-review-load".into())
            .spawn(move || {
                let res = backend.load(&project, &task, &workspace);
                let _ = tx.send(ReviewJobMsg::LoadDone(res));
            })
            .ok();
    }

    /// Act on a finished queued-review load. On success the slot is serving and
    /// the daemon's `ReviewOpened` event opens the interface; on failure clear
    /// the pending wait and surface the error (`rt-tui-review-load-queued`).
    fn on_review_load_done(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) => self.status = Some("review loaded; opening…".to_string()),
            Err(e) => {
                self.review_open_pending = None;
                self.status = Some(format!("review load failed: {e}"));
            }
        }
    }

    /// Open the review interface for a slot the daemon just reported serving.
    /// Only the client that loaded `task` opens it; a background `ReviewOpened`
    /// (another client's load, a poller resume) never steals this client's view,
    /// matching the tmux sidebar's no-focus resume (`rt-tui-review-load-queued`).
    fn on_review_opened(&mut self, task: String) {
        if self.review_open_pending.as_deref() == Some(task.as_str()) {
            self.review_open_pending = None;
            self.begin_review(task);
        }
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
            ReviewJobMsg::Resolved(result) => self.on_review_resolved(result),
            ReviewJobMsg::LoadDone(result) => self.on_review_load_done(result),
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
            ReviewAction::ShowDescription => {
                // Open the board's task-detail popover over the main area. The
                // overlay owns its own copy of the task, so the review content
                // session keeps running underneath (overlay and review are
                // independent fields). Esc / j/k drive the popover while it's up.
                if let Some(tf) = review.task().cloned() {
                    self.overlay = Some(ActiveOverlay::task_description(
                        tf,
                        self.keymaps.clone(),
                        self.display_style,
                    ));
                }
            }
            ReviewAction::OpenBrowser | ReviewAction::RevealFolder => {
                // Openers run inline (spawn+detach); failures land on the status.
                if let Some(r) = self.review.as_mut() {
                    r.set_status("opening…");
                }
                self.spawn_review_opener(action);
            }
            ReviewAction::Back => {
                // Leave the interface loaded (daemon sessions stay) and return to
                // the normal nav sidebar: drop the review panel, restore the view
                // that was active before the review opened, and put focus back on
                // the sidebar with its selection intact (opening a review never
                // changed the selection). `rt-review-screen-hangs-on-connecting`.
                self.review = None;
                let view = self.client.view().clone();
                self.apply_restored_view(view);
                self.client.focus_sidebar();
                self.dirty = true;
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
        // The teardown is deliberate — don't let the content supervisor restart
        // the sessions the Close op is about to end.
        if let Some(r) = self.review.as_mut() {
            r.note_deliberate_close();
        }
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
        if let Some(rx) = self.workers.as_ref().map(|w| &w.layout_rx) {
            // Drain until Empty or Disconnected (the subscriber thread only
            // disconnects on bundle drop; a switch replaces the whole bundle, so
            // there is no stale receiver to null out here).
            while let Ok(ev) = rx.try_recv() {
                events.push(ev);
            }
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
            // A slot this client loaded for review (`rt-tui-review-load-queued`)
            // is now serving: open the native interface. Only the initiating
            // client reacts (see `on_review_opened`), so a sibling's load or a
            // poller resume never steals this client's view.
            ReviewOpened { task, .. } => self.on_review_opened(task),
            // AgentRecovered: the content view reconnects on its own; a refresh
            // keeps the sidebar current. OrchestratorRestarted likewise needs
            // nothing here (the main session reconnects).
            ReviewAgentRecovered { .. } | OrchestratorRestarted => {}
        }
    }

    /// Whether the main area is currently showing the review for `task`.
    fn showing_review(&self, task: &str) -> bool {
        matches!(&self.main_view, MainView::Review(id) if id == task)
            && self.review.as_ref().map(|r| r.task_id()) == Some(task)
    }

    // -- workspace sidebar (the workspace-sidebar task) ---------------------

    /// Whether the dev workspace `name` currently has a running task, per the
    /// cached sidebar model (the same `current_task` signal the sidebar shows an
    /// agent badge for). Decides whether opening the row swaps in the workspace
    /// sidebar or keeps the nav sidebar with the idle placeholder.
    fn workspace_has_task(&self, name: &str) -> bool {
        self.sidebar_model
            .as_ref()
            .map(|m| {
                m.workspaces
                    .iter()
                    .any(|w| w.name == name && w.current_task.is_some())
            })
            .unwrap_or(false)
    }

    /// Begin opening the workspace sidebar for `name`: resolve its panel inputs
    /// (worktree, editor, in-progress task) off the UI thread, then build the
    /// interface when the resolve lands. Mirrors [`begin_review`](Self::begin_review).
    fn begin_workspace(&mut self, name: String) {
        if self.workspace.as_ref().map(|w| w.workspace()) == Some(name.as_str())
            && matches!(self.main_view, MainView::Workspace(_))
        {
            return;
        }
        let Some(project) = self.client.project().map(str::to_string) else {
            return;
        };
        self.workspace_opening = Some(name.clone());
        self.status = Some(format!("opening workspace {name}…"));
        let (tx, rx) = std::sync::mpsc::channel();
        self.workspace_rx = Some(rx);
        std::thread::Builder::new()
            .name("shelbi-workspace-open".into())
            .spawn(move || {
                let res = shelbi_orchestrator::workspace_session::workspace_open_info(
                    &project, &name,
                )
                .map(|info| WorkspaceOpenParams {
                    workspace: name.clone(),
                    worktree: info.worktree,
                    editor_name: info.editor_name,
                    task: info.task,
                })
                .map_err(|e| e.to_string());
                let _ = tx.send(WorkspaceJobMsg::Opened(res));
            })
            .ok();
    }

    /// Drain the workspace background channel: build the interface on a resolve,
    /// or bind the ensured content session.
    fn poll_workspace(&mut self) -> bool {
        let msg = match &self.workspace_rx {
            Some(rx) => match rx.try_recv() {
                Ok(m) => m,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => {
                    self.workspace_rx = None;
                    return false;
                }
            },
            None => return false,
        };
        self.workspace_rx = None;
        match msg {
            WorkspaceJobMsg::Opened(res) => self.on_workspace_opened(res),
            WorkspaceJobMsg::ContentReady(role, Ok(())) => {
                if let Some(w) = self.workspace.as_mut() {
                    w.show_role(role);
                    self.reported_main = None;
                }
            }
            WorkspaceJobMsg::ContentReady(_role, Err(e)) => {
                if let Some(w) = self.workspace.as_mut() {
                    w.show_agent();
                    w.set_status(format!("couldn't open view: {e}"));
                }
            }
        }
        true
    }

    /// Act on a finished workspace-open resolve. A resolve for a workspace the
    /// user is no longer opening is dropped.
    fn on_workspace_opened(&mut self, res: Result<WorkspaceOpenParams, String>) {
        if self.workspace_opening.take().is_none() {
            return; // navigated away / superseded
        }
        match res {
            Ok(p) => self.build_workspace_interface(p),
            Err(e) => self.status = Some(format!("workspace failed: {e}")),
        }
    }

    /// Build the native workspace interface from resolved params and switch the
    /// main area to it.
    fn build_workspace_interface(&mut self, p: WorkspaceOpenParams) {
        let project = self
            .client
            .project()
            .map(str::to_string)
            .unwrap_or_default();
        let task = p.task.map(|b| *b);
        let agent_name = task
            .as_ref()
            .map(agent_display_name)
            .unwrap_or_else(|| "Agent".to_string());
        let status = task
            .as_ref()
            .map(|t| status_for_column(&t.task.column))
            .unwrap_or_else(default_ws_status);
        self.main_view = MainView::Workspace(p.workspace.clone());
        self.client.focus_main();
        self.workspace = Some(WorkspaceInterface::new(
            &project,
            self.connector.clone(),
            p.workspace,
            p.worktree,
            p.editor_name,
            agent_name,
            status,
            task,
        ));
        self.reported_main = None;
        self.status = None;
        self.dirty = true;
    }

    /// Carry out one [`WorkspaceAction`] from the embedded panel. Blocking parts
    /// run off the UI thread.
    fn apply_workspace_action(&mut self, action: WorkspaceAction) {
        let (Some(project), Some(ws)) = (
            self.client.project().map(str::to_string),
            self.workspace.as_ref(),
        ) else {
            return;
        };
        let name = ws.workspace().to_string();
        match action {
            WorkspaceAction::None => {}
            WorkspaceAction::ShowContent(None) => {
                if let Some(w) = self.workspace.as_mut() {
                    w.show_agent();
                    self.reported_main = None;
                }
            }
            WorkspaceAction::ShowContent(Some(role)) => {
                // Ask the daemon to ensure the editor/diff session, then bind it.
                let (tx, rx) = std::sync::mpsc::channel();
                self.workspace_rx = Some(rx);
                let name = name.clone();
                std::thread::Builder::new()
                    .name("shelbi-workspace-ensure".into())
                    .spawn(move || {
                        let op = shelbi_app::WorkspaceSessionOp::Ensure { role };
                        let res = shelbi_app::workspace_session(&project, &name, op, &mut |_, _| {})
                            .map_err(|e| e.to_string());
                        let _ = tx.send(WorkspaceJobMsg::ContentReady(role, res));
                    })
                    .ok();
            }
            WorkspaceAction::RevealFolder => {
                if let Some(w) = self.workspace.as_mut() {
                    w.set_status("opening…");
                }
                self.spawn_workspace_reveal(name);
            }
            WorkspaceAction::ShowDescription => {
                if let Some(tf) = ws.task().cloned() {
                    self.overlay = Some(ActiveOverlay::task_description(
                        tf,
                        self.keymaps.clone(),
                        self.display_style,
                    ));
                }
            }
            WorkspaceAction::Back => {
                // Leave the content sessions loaded and return to the nav sidebar,
                // restoring the view active before the workspace opened.
                self.workspace = None;
                let view = self.client.view().clone();
                self.apply_restored_view(view);
                self.client.focus_sidebar();
                self.dirty = true;
            }
            WorkspaceAction::Close => self.close_workspace(&project, &name),
        }
    }

    /// Reveal the workspace worktree folder, off the UI thread.
    fn spawn_workspace_reveal(&mut self, workspace: String) {
        let Some(project) = self.client.project().map(str::to_string) else {
            return;
        };
        self.spawn_job(move || match workspace::reveal_folder(&project, &workspace) {
            Ok(()) => "revealed folder".to_string(),
            Err(e) => format!("reveal failed: {e}"),
        });
    }

    /// Close the workspace sidebar: ask the daemon to end the editor/diff
    /// sessions, drop the interface, and return to the nav sidebar.
    fn close_workspace(&mut self, project: &str, workspace: &str) {
        // Deliberate teardown — the content supervisor must not restart the
        // sessions the Close op ends.
        if let Some(w) = self.workspace.as_mut() {
            w.note_deliberate_close();
        }
        let project = project.to_string();
        let workspace = workspace.to_string();
        self.spawn_job(move || {
            let op = shelbi_app::WorkspaceSessionOp::Close;
            match shelbi_app::workspace_session(&project, &workspace, op, &mut |_, _| {}) {
                Ok(()) => "workspace view closed".to_string(),
                Err(e) => format!("workspace close failed: {e}"),
            }
        });
        self.workspace = None;
        let view = self.client.view().clone();
        self.apply_restored_view(view);
        self.client.focus_sidebar();
        self.dirty = true;
    }

    fn handle_sidebar_key(&mut self, k: KeyEvent) {
        let count = self
            .sidebar_view()
            .map(|v| v.selectable_count())
            .unwrap_or(0);
        match k.code {
            // Arrow keys and Vim j/k move the selection only — they never open
            // the row or move focus to the main area (the main view stays put;
            // it changes only on open, below).
            KeyCode::Up | KeyCode::Char('k') => self.client.select_up(),
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => self.client.select_down(count),
            KeyCode::BackTab => self.client.select_up(),
            // Space or Enter opens the selected row and moves focus to the main
            // area (`activate_selection` → `show`, which focuses main for every
            // row type).
            KeyCode::Enter | KeyCode::Char(' ') => self.activate_selection(),
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
        // The workspace interface owns the main area when it is open (the
        // workspace-sidebar task).
        if matches!(self.main_view, MainView::Workspace(_)) {
            if let Some(w) = self.workspace.as_mut() {
                let action = w.handle_key(k);
                self.apply_workspace_action(action);
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

        // Cmd+C / Ctrl+Shift+C copies the current selection to the clipboard
        // and is consumed here — never forwarded — so no stray `c` / Ctrl+C
        // reaches the agent. With no selection it is a harmless no-op (we still
        // swallow it rather than risk sending an interrupt).
        if terminal_view::is_copy_key(&k) {
            if let Some(text) = self.sessions.live_pane_mut().and_then(|p| p.selection_copy()) {
                copy_to_clipboard(&text);
            }
            self.dirty = true;
            return;
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
            KeyCode::Char('n') => p.search_older(),
            KeyCode::Char('N') => p.search_newer(),
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
        use crossterm::event::{MouseButton, MouseEventKind};
        // Hover tracking for the drag-handle line: any pointer motion over the
        // divider column lights the handle; motion off it dims it again. Only
        // flip (and repaint) when the state actually changes so the common case
        // — motion that never touches the column — stays free. This runs ahead
        // of the routing below and falls through, so motion is still forwarded
        // to a live pane as before.
        if matches!(m.kind, MouseEventKind::Moved) {
            let hovering = self.on_divider(m.column, m.row);
            if hovering != self.divider_hover {
                self.divider_hover = hovering;
                self.dirty = true;
            }
        }
        // The sidebar/main divider drag takes priority over everything else,
        // including the review's mouse routing: while a review is open the panel
        // sits in the sidebar's column, so the divider between panel and content
        // is still draggable and a divider press must neither select a panel
        // item nor reach the content view.
        //
        // An in-progress divider drag captures all mouse motion until the
        // button releases, wherever the pointer travels.
        if self.sidebar_dragging {
            match m.kind {
                MouseEventKind::Drag(MouseButton::Left)
                | MouseEventKind::Down(MouseButton::Left) => self.update_sidebar_drag(m.column),
                MouseEventKind::Up(MouseButton::Left) => self.end_sidebar_drag(),
                _ => {}
            }
            return;
        }
        // A press on the divider column starts a drag — and is swallowed so it
        // never selects a sidebar/panel row or reaches the main/content pane.
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && self.on_divider(m.column, m.row)
        {
            self.sidebar_dragging = true;
            self.update_sidebar_drag(m.column);
            return;
        }
        // While a review is open the panel occupies the sidebar column and the
        // content view the main area, so the review interface owns clicks in
        // both rects — except the divider press-drag-release handled above
        // (`rt-review-screen-hangs-on-connecting`).
        if matches!(self.main_view, MainView::Review(_)) {
            if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.client.focus_main();
            }
            let panel_rect = self.sidebar_content_rect();
            let main_rect = self.main_rect;
            if let Some(r) = self.review.as_mut() {
                let action = r.handle_mouse(m, panel_rect, main_rect);
                self.apply_review_action(action);
                self.dirty = true;
            }
            return;
        }
        // The workspace interface owns clicks in both columns, same as review.
        if matches!(self.main_view, MainView::Workspace(_)) {
            if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.client.focus_main();
            }
            let panel_rect = self.sidebar_content_rect();
            let main_rect = self.main_rect;
            if let Some(w) = self.workspace.as_mut() {
                let action = w.handle_mouse(m, panel_rect, main_rect);
                self.apply_workspace_action(action);
                self.dirty = true;
            }
            return;
        }
        if contains(self.sidebar_rect, m.column, m.row) {
            self.handle_sidebar_mouse(m);
            return;
        }
        if contains(self.main_rect, m.column, m.row) {
            self.handle_main_mouse(m);
        }
    }

    /// The divider column: the sidebar's rightmost column, i.e. the visual
    /// border between the sidebar and the main pane.
    fn divider_col(&self) -> u16 {
        self.sidebar_rect.right().saturating_sub(1)
    }

    /// The sidebar area minus its rightmost column, which is reserved for the
    /// drag-handle line. Sidebar/review content renders into — and hit-tests
    /// against — this narrower rect so nothing paints over or clips the line.
    fn sidebar_content_rect(&self) -> Rect {
        Rect {
            width: self.sidebar_rect.width.saturating_sub(1),
            ..self.sidebar_rect
        }
    }

    /// The full shell area: the sidebar and the main pane combined (below the
    /// lost-login banner, if any). Overlays center on this so a popover sits in
    /// the middle of the whole window rather than off to the right in the main
    /// area. The two rects share a top and height, so the union is the sidebar's
    /// origin spanning both widths.
    fn full_area(&self) -> Rect {
        Rect {
            x: self.sidebar_rect.x,
            y: self.sidebar_rect.y,
            width: self.sidebar_rect.width + self.main_rect.width,
            height: self.main_rect.height.max(self.sidebar_rect.height),
        }
    }

    /// Whether `(col, row)` lands on the draggable divider between the sidebar
    /// and the main pane.
    fn on_divider(&self, col: u16, row: u16) -> bool {
        let r = self.sidebar_rect;
        r.width > 0
            && r.height > 0
            && col == self.divider_col()
            && row >= r.top()
            && row < r.bottom()
    }

    /// Update the sidebar width from a drag pointing at `pointer_col`. The
    /// sidebar's right edge follows the pointer (the pointer column is the
    /// sidebar's new last column), clamped to [24, half the window]. The live
    /// value drives the next `draw`, which reflows the main pane and resizes
    /// the attached session; the choice is persisted on release.
    fn update_sidebar_drag(&mut self, pointer_col: u16) {
        let window_width = self.sidebar_rect.width + self.main_rect.width;
        let desired = pointer_col.saturating_add(1);
        let clamped = shelbi_app::nav::clamp_sidebar_width(desired, window_width);
        self.client.set_sidebar_width(clamped);
        self.dirty = true;
    }

    /// End a divider drag and persist the chosen width so it survives a
    /// restart. Best-effort: a disk failure surfaces in the status line but
    /// never crashes the UI (the width just won't be remembered).
    fn end_sidebar_drag(&mut self) {
        self.sidebar_dragging = false;
        if let Err(e) = shelbi_state::set_sidebar_width(self.client.sidebar_width()) {
            self.status = Some(format!("sidebar width save failed: {e}"));
        }
        self.dirty = true;
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
                // A click on a selectable row focuses the sidebar, selects that
                // row, and opens it — the same one-click behavior as pressing
                // Enter on it. A click on a section header or blank space only
                // focuses the sidebar (hit returns `None`), opening nothing.
                self.client.focus_sidebar();
                if let Some(view) = self.sidebar_view() {
                    // A click on the search box opens the command palette, the
                    // same as pressing the palette chord.
                    if view.search_box_hit(self.sidebar_content_rect(), m.column, m.row) {
                        self.open_palette();
                        self.dirty = true;
                        return;
                    }
                    if let Some(sel) = view.hit(self.sidebar_content_rect(), m.column, m.row) {
                        self.client.clamp_selection(view.selectable_count());
                        // Move selection to the clicked row.
                        while self.client.sidebar_selection() < sel {
                            self.client.select_down(view.selectable_count());
                        }
                        while self.client.sidebar_selection() > sel {
                            self.client.select_up();
                        }
                        // Open it, exactly as Enter would: `show` opens the row
                        // and moves focus to the main area for every row type —
                        // native view, session, review, or workspace — so the
                        // next keystroke drives what was opened.
                        if let Some(target) = view.target_at(sel) {
                            self.show(target);
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
        // A click in the main area focuses it. (An open review's clicks are
        // routed in `handle_mouse` before reaching here, across both columns.)
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.client.focus_main();
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
        // Taking focus takes the session's size, as the desktop pane does: a
        // taller client that typed last stops sizing what we're looking at.
        if focused {
            self.sessions.claim_size();
        }
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

/// The Orchestrator view's placeholder while its session is down and the daemon
/// supervisor has reported why (`orch-relaunch-noop-after-session-death`): the
/// supervisor's reason replaces a bare "Connecting…" / attach error, so a view
/// that won't load says what failed. `None` for any other target, a live (or
/// idle) session, or no supervisor report.
fn orchestrator_down_notice(
    target: Option<&SessionRef>,
    state: &MainState<'_>,
    supervisor_status: Option<&str>,
) -> Option<String> {
    if target != Some(&SessionRef::Orchestrator)
        || matches!(state, MainState::Live(_) | MainState::Idle(_))
    {
        return None;
    }
    supervisor_status.map(shelbi_app::session::orchestrator_down_notice)
}

fn draw(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut ShellState,
) -> Result<()> {
    // Bracket the frame in a synchronized update so the terminal never shows
    // it half-painted.
    let begin = execute!(io::stdout(), BeginSynchronizedUpdate);
    let res = draw_frame(term, state);
    if begin.is_ok() {
        let _ = execute!(io::stdout(), EndSynchronizedUpdate);
    }
    res
}

/// Paint one frame of the shell on `term`: the production terminal, or a
/// `TestBackend` in the smoke tests.
fn draw_frame<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    state: &mut ShellState,
) -> Result<()> {
    // Compute layout up front so we can report the main-area size to the live
    // session before painting.
    let full = Rect {
        x: 0,
        y: 0,
        width: term.size()?.width,
        height: term.size()?.height,
    };
    // A persistent, full-width banner when the daemon has lost its macOS login
    // session — shown in every view (the sidebar footer is too narrow and is
    // suppressed under a review/workspace panel). Carve one row off the top and
    // lay the rest of the shell out below it.
    let session_lost_banner = state
        .sidebar_model
        .as_ref()
        .and_then(|m| m.session_lost.clone());
    let (banner_rect, area) = match &session_lost_banner {
        Some(_) if full.height > 1 => (
            Some(Rect {
                x: 0,
                y: 0,
                width: full.width,
                height: 1,
            }),
            Rect {
                x: 0,
                y: 1,
                width: full.width,
                height: full.height - 1,
            },
        ),
        _ => (None, full),
    };
    // Clamp the saved width to the live window for display only (at least 24,
    // at most half the window). A narrow window shrinks the sidebar on screen
    // without touching the saved value, so widening restores the user's choice.
    let display_width = shelbi_app::nav::clamp_sidebar_width(state.sidebar_width(), area.width);
    let (sidebar_rect, main_rect) = layout(area, display_width);
    state.sidebar_rect = sidebar_rect;
    state.main_rect = main_rect;
    // The sidebar/review content stops one column short of the sidebar's right
    // edge; that last column carries the drag-handle line (drawn after the
    // content, below), lit while the pointer hovers it or a resize is underway.
    let content_rect = state.sidebar_content_rect();
    let divider_active = state.sidebar_dragging || state.divider_hover;

    // Report our viewport so the session reflows to fill the main area when we
    // are the most-recently-active client.
    let main_size = Size::new(main_rect.width, main_rect.height);
    if state.reported_main != Some(main_size) {
        state.sessions.resize(main_size.cols, main_size.rows);
        // The review content view fills the whole main area (the panel now sits
        // in the sidebar column), so it reflows to the same `main_rect`.
        if let Some(r) = state.review.as_mut() {
            r.resize(main_rect);
        }
        if let Some(w) = state.workspace.as_mut() {
            w.resize(main_rect);
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
    // Likewise for the workspace interface (the workspace-sidebar task).
    let mut workspace = state.workspace.take();

    // Borrow what the closure needs. `sidebar_view` is owned and `main_view` is
    // cloned (cheap) so the closure can disjointly borrow the embedded views
    // mutably — a native view's `render_full` takes `&mut app` and `&mut Frame`.
    let sidebar_view = state.sidebar_view();
    let sidebar_chrome = state.sidebar_chrome();
    // The daemon supervisor's reason a dead orchestrator isn't coming back, for
    // the Orchestrator view's placeholder.
    let orchestrator_status = state
        .sidebar_model
        .as_ref()
        .and_then(|m| m.orchestrator_status.clone());
    let main_view = state.main_view.clone();
    let is_review = matches!(main_view, MainView::Review(_));
    let is_workspace = matches!(main_view, MainView::Workspace(_));
    // Either sidebar-column interface suppresses the nav sidebar and the shell's
    // bottom status band (each carries its own column + status line).
    let is_panel = is_review || is_workspace;
    let kanban = &mut state.kanban;
    let activity = &mut state.activity;
    let machines = &mut state.machines;
    let sessions = &state.sessions;

    let mut cursor: Option<(u16, u16)> = None;
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

            // The lost-login-session banner, above everything (its row was
            // carved off the top of the layout).
            if let (Some(rect), Some(msg)) = (banner_rect, session_lost_banner.as_deref()) {
                render_session_lost_banner(buf, rect, msg);
            }

            // Sidebar — the nav sidebar, except while a review or workspace
            // interface is open: then that panel takes the sidebar's column
            // (rendered at the frame level below), so there are only two columns,
            // panel and content (`rt-review-screen-hangs-on-connecting`).
            if !is_panel {
                if let Some(view) = &sidebar_view {
                    view.render(buf, content_rect, selection, !focus_main, &sidebar_chrome);
                }
            }

            // Main area (session / review-fallback; native views drawn above).
            let orch_down = orchestrator_down_notice(
                sessions.current_target(),
                &sessions.state(),
                orchestrator_status.as_deref(),
            );
            match &main_view {
                MainView::Session if orch_down.is_some() => {
                    render_placeholder(buf, main_rect, orch_down.as_deref().unwrap_or_default())
                }
                MainView::Session => match sessions.state() {
                    MainState::Empty => render_placeholder(buf, main_rect, "No session"),
                    MainState::Connecting(r) => {
                        render_placeholder(buf, main_rect, &shelbi_app::session::connecting_notice(r))
                    }
                    MainState::Idle(info) => {
                        render_idle_workspace(buf, main_rect, info, sidebar_chrome.palette())
                    }
                    MainState::Failed(r, err) => render_placeholder(
                        buf,
                        main_rect,
                        &shelbi_app::session::failed_notice(r, err),
                    ),
                    MainState::Restarting(attempt, max) => render_placeholder(
                        buf,
                        main_rect,
                        &shelbi_app::session::restarting_notice(attempt, max),
                    ),
                    MainState::GaveUp(last_line) => {
                        render_placeholder(buf, main_rect, &gave_up_notice(last_line))
                    }
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
                MainView::Workspace(name) => {
                    // The interface renders at the frame level below; this is only
                    // the "still opening" fallback before it is built.
                    if workspace.is_none() {
                        render_placeholder(buf, main_rect, &format!("opening workspace {name}…"));
                    }
                }
            }

            // The review / workspace panel carries its own status line, so the
            // shell's bottom-row status/search band is only for the other views.
            if !is_panel {
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

        // The review interface renders with a mutable frame borrow: the panel
        // into the sidebar's column (`sidebar_rect`), the content terminal view
        // into the main area (`main_rect`) — two columns, panel and content.
        if let Some(r) = review.as_mut() {
            let cur = r.render(frame, content_rect, main_rect, truecolor, focus_main);
            if overlay.is_none() {
                cursor = cur;
            }
        }

        // The workspace interface renders the same way — panel into the sidebar
        // column, content terminal view into the main area.
        if let Some(w) = workspace.as_mut() {
            let cur = w.render(frame, content_rect, main_rect, truecolor, focus_main);
            if overlay.is_none() {
                cursor = cur;
            }
        }

        // The drag-handle line down the sidebar's right edge, painted last so
        // neither the nav sidebar nor the review panel (whichever holds that
        // column) can cover it. Its column was kept clear by rendering the
        // content one column narrower above.
        render_divider(frame.buffer_mut(), sidebar_rect, divider_active);

        // Dim the main area under an open overlay so the modal reads as on top
        // (the overlay's own `Clear` un-dims the cells it occupies).
        if overlay.is_some() {
            dim_area(frame.buffer_mut(), main_rect);
        }

        // An open overlay draws over the dimmed main area and owns the cursor.
        // It centers on the full window (sidebar + main), so it is handed `area`
        // (both columns) rather than just `main_rect`; the sidebar was drawn
        // above and the overlay's own `Clear` covers it where they overlap.
        if let Some(ov) = overlay.as_mut() {
            ov.render(frame, area);
        } else if let Some((x, y)) = cursor {
            frame.set_cursor_position(Position::new(x, y));
        }
    });
    state.overlay = overlay;
    state.review = review;
    state.workspace = workspace;
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

/// Paint the resize drag handle: a full-height vertical rule down the
/// sidebar's rightmost column (`sidebar_rect.right() - 1`, the exact column a
/// drag starts from). Dim by default, cyan/bold when `active` (the pointer is
/// hovering it or a drag is in progress). Drawn after the sidebar/review
/// content, whose own render was narrowed by one column so it never lands here.
fn render_divider(buf: &mut ratatui::buffer::Buffer, sidebar_rect: Rect, active: bool) {
    if sidebar_rect.width == 0 || sidebar_rect.height == 0 {
        return;
    }
    let x = sidebar_rect.right().saturating_sub(1);
    // The divider column keeps the terminal's default (transparent) background,
    // like the rest of the sidebar — only the thin `▏` rule glyph is painted, so
    // a semi-transparent terminal shows through behind it.
    let style = if active {
        Style::default()
            .fg(palette().divider_active)
            .bg(Color::Reset)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(palette().divider)
            .bg(Color::Reset)
    };
    for y in sidebar_rect.top()..sidebar_rect.bottom() {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(crate::theme::DIVIDER_GLYPH);
            cell.set_style(style);
        }
    }
}

fn render_status(buf: &mut ratatui::buffer::Buffer, area: Rect, text: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let y = area.bottom() - 1;
    let style = Style::default()
        .bg(palette().status_bar)
        .fg(palette().ansi_gray);
    let label = format!(" {text} ");
    for (x, ch) in (area.left()..area.right()).zip(label.chars().chain(std::iter::repeat(' '))) {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

/// Paint the lost-login-session banner across `area` (a single full-width row):
/// a red band with bold white text, truncated to the row width. Loud on purpose
/// — it means every agent the daemon starts has broken DNS/SSH/user lookups.
fn render_session_lost_banner(buf: &mut ratatui::buffer::Buffer, area: Rect, msg: &str) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let style = Style::default()
        .bg(palette().ansi_red)
        .fg(palette().on_color)
        .add_modifier(Modifier::BOLD);
    let label = format!(" {msg} ");
    let y = area.top();
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
        // A review / workspace content session (editor/diff) is transient and
        // laid out from current state, never restored from a saved view.
        RowTarget::Session(SessionRef::Review { .. })
        | RowTarget::Session(SessionRef::WorkspaceContent { .. })
        | RowTarget::Review(_) => None,
        // A machine group header toggles collapse; it is not a view.
        RowTarget::Machine(_) => None,
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

/// Default agent shown when a task has no explicit `agent:` — matches the
/// sidebar's `DEFAULT_TASK_AGENT`.
const DEFAULT_TASK_AGENT: &str = "developer";

/// The agent's display name for the workspace panel's Agent switch — the task's
/// `agent:` param (defaulting to `developer`), title-cased like the sidebar's
/// right-column agent label (`developer` → `Developer`).
fn agent_display_name(task: &shelbi_state::IssueFile) -> String {
    let key = task
        .task
        .param_str("agent")
        .unwrap_or(DEFAULT_TASK_AGENT);
    shelbi_app::view::title_case(key)
}

/// The status label + colour the workspace panel shows beside the back button,
/// derived from the task's column. The workspace sidebar opens only for an
/// in-progress workspace, so `IN PROGRESS` (bold yellow) is the usual case; the
/// other arms keep it robust if the column changes under an open sidebar.
fn status_for_column(column: &shelbi_core::Column) -> crate::workspace_panel::WsStatus {
    use crate::theme::palette;
    if *column == shelbi_core::Column::in_progress() {
        crate::workspace_panel::WsStatus {
            text: "IN PROGRESS".into(),
            color: palette().yellow,
        }
    } else if *column == shelbi_core::Column::review() {
        crate::workspace_panel::WsStatus {
            text: "READY FOR REVIEW".into(),
            color: palette().cyan,
        }
    } else if *column == shelbi_core::Column::done() {
        crate::workspace_panel::WsStatus {
            text: "DONE".into(),
            color: palette().green,
        }
    } else {
        crate::workspace_panel::WsStatus {
            text: column.as_str().to_uppercase(),
            color: palette().muted,
        }
    }
}

/// The workspace panel's header status for `workspace`, derived from the live
/// sidebar model — the same source the sidebar badge reads. A workspace with a
/// current in-progress task reads `IN PROGRESS`; one with none reads `IDLE`.
/// Used to firm up an open panel's header on each board refresh so it tracks the
/// sidebar (and the issue board) rather than freezing at the value it held when
/// the view opened. The workspace sidebar opens only for an in-progress
/// workspace, so `IN PROGRESS` is the usual case; a handoff that clears the
/// assignment idles the header the same tick the sidebar badge goes idle.
fn ws_status_from_model(
    model: &SidebarModel,
    workspace: &str,
) -> crate::workspace_panel::WsStatus {
    let has_task = model
        .workspaces
        .iter()
        .any(|w| w.name == workspace && w.current_task.is_some());
    if has_task {
        status_for_column(&shelbi_core::Column::in_progress())
    } else {
        default_ws_status()
    }
}

/// The fallback status when the workspace has no resolvable task (the sidebar
/// should not open in that case, but the panel degrades gracefully).
fn default_ws_status() -> crate::workspace_panel::WsStatus {
    crate::workspace_panel::WsStatus {
        text: "IDLE".into(),
        color: palette().muted,
    }
}

/// The lines of the idle-workspace placeholder
/// ([`shelbi_app::session::idle_placeholder_lines`], shared with the desktop's
/// Workspace view).
fn idle_placeholder_lines(info: &session::IdleInfo, palette_chord: &str) -> Vec<String> {
    shelbi_app::session::idle_placeholder_lines(info, palette_chord)
}

/// Render the idle-workspace placeholder (`rt-tui-idle-workspace-open`): the
/// workspace's identity and how to start work on it, top-left per the Figma
/// (`workspace-idle`). The name reads bold white, the `machine · branch` line and
/// the body read muted.
fn render_idle_workspace(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    info: &session::IdleInfo,
    palette_chord: &str,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = idle_placeholder_lines(info, palette_chord);
    let n = lines.len() as u16;
    let body: Vec<Line> = lines
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            let style = if i == 0 {
                // Name: bold white.
                Style::default().fg(palette().ansi_white).add_modifier(Modifier::BOLD)
            } else {
                // Identity + body: muted (`#7a7a7a`).
                Style::default().fg(palette().muted)
            };
            Line::from(Span::styled(text, style))
        })
        .collect();
    // Top-left, with a one-column gutter.
    let block = Rect::new(
        area.x + 1,
        area.y,
        area.width.saturating_sub(2),
        n.min(area.height),
    );
    Paragraph::new(body)
        .wrap(Wrap { trim: true })
        .render(block, buf);
}

fn render_placeholder(buf: &mut ratatui::buffer::Buffer, area: Rect, text: &str) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let p = Paragraph::new(vec![Line::from(Span::styled(
        text,
        Style::default().fg(palette().ansi_dark_gray),
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
        .bg(palette().notice)
        .fg(palette().on_color);
    let label = format!(" {text} ");
    for (x, ch) in (area.left()..area.right()).zip(label.chars()) {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_style(style);
        }
    }
}

// --- the background model reader -------------------------------------------

/// Read a fresh core snapshot for `project`. Runs on the core refresher thread
/// (never the UI thread): the sidebar model plus the opaque data bundles the
/// embedded board and machines views fold in. The board read goes through the
/// daemon-owned index (no per-pane `gh` sweep); machines reads the poller's
/// persisted state — both fast, disk/index reads. A slow read here never blocks
/// the event loop (the loop only polls [`refresh::Refresher::latest`]).
///
/// The activity feed is **not** read here: its title resolution is live `gh`
/// traffic that can take seconds on a large closed history, so it runs on a
/// separate refresher (see [`run_with`]) and never gates this snapshot — the fix
/// for the sidebar stalling behind a done-history hydration sweep.
fn read_snapshot(project: &str, _generation: u64) -> ShellSnapshot {
    ShellSnapshot {
        sidebar: shelbi_orchestrator::sidebar_model::read_sidebar_model(project),
        board: Some(KanbanApp::read_board_data(project)),
        activity: None,
        machines: Some(MachinesApp::read_data(project)),
    }
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

/// Raw mode, the alternate screen, mouse/focus/paste capture and the kitty
/// keyboard push: what [`RawGuard::enter`] sets up and [`restore_terminal`]
/// undoes. Also re-entered after an external editor had the terminal.
fn enter_terminal_modes() -> io::Result<()> {
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
    Ok(())
}

/// Run the user's editor (`shelbi_state::resolve_editor`: the config's
/// `editor`, then `$EDITOR`, then vim) on `path` in the foreground: leave the
/// shell's terminal modes, wait for the editor, restore them, and force a full
/// repaint. Returns the status line to show. The loop's input reader is on
/// this thread, so nothing competes with the editor for keys.
fn run_editor(term: &mut Terminal<CrosstermBackend<io::Stdout>>, path: &std::path::Path) -> String {
    let editor = shelbi_state::resolve_editor();
    let mut parts = editor.split_whitespace();
    let program = parts.next().unwrap_or("vim").to_string();
    restore_terminal();
    let outcome = std::process::Command::new(&program)
        .args(parts)
        .arg(path)
        .status();
    let _ = enter_terminal_modes();
    let _ = term.clear();
    match outcome {
        Ok(status) if status.success() => format!("edited {}", path.display()),
        Ok(status) => format!("{program} exited with {status}"),
        Err(e) => format!("couldn't start the editor `{editor}`: {e}"),
    }
}

impl RawGuard {
    fn enter() -> Result<Self> {
        enter_terminal_modes()?;
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

    #[test]
    fn orchestrator_view_names_the_supervisor_failure_while_down() {
        let orch = SessionRef::Orchestrator;
        let status = Some("supervisor gave up: spawn refused");
        let notice = orchestrator_down_notice(Some(&orch), &MainState::Connecting(&orch), status)
            .expect("a down orchestrator with a supervisor report names it");
        assert!(notice.contains("supervisor gave up: spawn refused"), "{notice}");
        assert!(
            orchestrator_down_notice(Some(&orch), &MainState::GaveUp("boom"), status).is_some(),
            "the client's own give-up defers to the supervisor's reason",
        );
        // No report, or not the orchestrator: the normal placeholder stands.
        assert_eq!(orchestrator_down_notice(Some(&orch), &MainState::Connecting(&orch), None), None);
        let ws = SessionRef::Workspace("alpha".into());
        assert_eq!(orchestrator_down_notice(Some(&ws), &MainState::Connecting(&ws), status), None);
        assert_eq!(orchestrator_down_notice(None, &MainState::Empty, status), None);
    }

    struct NoopConnector;
    impl session::Connector for NoopConnector {
        fn connect(
            &self,
            _p: &str,
            _t: &SessionRef,
        ) -> Result<session::Connected, session::ConnectFailure> {
            Err(session::ConnectFailure::Message("no session in tests".into()))
        }
    }

    /// A connector that reports every workspace target as idle, with the given
    /// identity — for the idle-workspace-open tests (`rt-tui-idle-workspace-open`).
    struct IdleWorkspaceConnector {
        machine: String,
        branch: Option<String>,
    }
    impl session::Connector for IdleWorkspaceConnector {
        fn connect(
            &self,
            _p: &str,
            target: &SessionRef,
        ) -> Result<session::Connected, session::ConnectFailure> {
            match target {
                SessionRef::Workspace(name) => {
                    Err(session::ConnectFailure::Idle(session::IdleInfo {
                        name: name.clone(),
                        machine: self.machine.clone(),
                        branch: self.branch.clone(),
                    }))
                }
                _ => Err(session::ConnectFailure::Message("no session in tests".into())),
            }
        }
    }

    fn test_state() -> ShellState {
        test_state_with_connector(Arc::new(NoopConnector))
    }

    /// A [`WorkersFactory`] that resolves no SHELBI_HOME and opens no hub socket:
    /// a refresher whose reader returns an empty snapshot and dummy, immediately-
    /// closed receivers with no subscriber threads. Installed on every test state
    /// so a `switch_project` under `cfg(test)` rebuilds a safe bundle instead of
    /// the production one. Each build appends the requested project to `log`, so a
    /// test can assert the refresh source follows the current project.
    fn recording_workers_factory(log: Arc<Mutex<Vec<String>>>) -> WorkersFactory {
        Arc::new(move |project: &str| {
            log.lock().unwrap().push(project.to_string());
            let refresher = refresh::spawn(|_g| ShellSnapshot {
                sidebar: None,
                board: None,
                activity: None,
                machines: None,
            });
            // The activity refresher tags its read with the project, so a test can
            // read back (via `latest_activity`) which project it followed.
            let proj = project.to_string();
            let activity_refresher =
                refresh::spawn(move |_g| ActivityData::tagged_for_test(&proj));
            // The closed refresher touches no backend in tests — an empty page.
            let closed_refresher = refresh::spawn(|_g| None);
            let (_ctx, change_rx) = std::sync::mpsc::channel();
            let (_ltx, layout_rx) = std::sync::mpsc::channel();
            ProjectWorkers {
                refresher,
                activity_refresher,
                closed_refresher,
                change_rx,
                layout_rx,
                _changes: None,
                _layout_sub: None,
            }
        })
    }

    /// A snapshot whose sidebar names `project`, so a test can tell which
    /// project's refresher produced it.
    fn tagged_snapshot(project: &str) -> ShellSnapshot {
        ShellSnapshot {
            sidebar: Some(SidebarModel {
                project_label: project.to_string(),
                nav: vec![],
                workspaces: vec![],
                reviews: vec![],
                config_error: None,
                board_loading: false,
                collapsed_machines: Default::default(),
                board_banner: None,
                daemon_version_line: None,
                daemon_version_mismatch: false,
                status_line: String::new(),
                zen_mode: shelbi_state::ZenModeState::Off,
                unread_errors: 0,
                session_lost: None,
                orchestrator_status: None,
            }),
            board: None,
            activity: None,
            machines: None,
        }
    }

    fn test_state_with_connector(connector: Arc<dyn session::Connector>) -> ShellState {
        let caps = Caps {
            kitty: true,
            truecolor: true,
            nested: None,
        };
        let mut st = ShellState::new("proj", connector, caps);
        // Keep `switch_project` (and any startup) from touching the hub socket or
        // resolving SHELBI_HOME in tests. Tests that care about which project the
        // refresher follows install their own recording factory + log.
        st.make_workers = recording_workers_factory(Arc::new(Mutex::new(Vec::new())));
        st.apply_snapshot(ShellSnapshot {
            sidebar: Some(SidebarModel {
                project_label: "proj".into(),
                nav: vec![
                    NavItem {
                        label: "Orchestrator".into(),
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
                    machine: "hub".into(),
                    is_remote: false,
                    current_task: None,
                    agent: None,
                    badge: shelbi_app::view::WorkspaceBadge::Idle,
                }],
                reviews: vec![],
                config_error: None,
                board_loading: false,
                collapsed_machines: Default::default(),
                board_banner: None,
                daemon_version_line: None,
                daemon_version_mismatch: false,
                status_line: String::new(),
                zen_mode: shelbi_state::ZenModeState::Off,
                unread_errors: 0,
                session_lost: None,
                orchestrator_status: None,
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
    fn ctrl_h_focuses_the_sidebar_and_ctrl_l_the_main_pane() {
        // The vim-style focus moves work from either side, from any main view.
        let mut st = test_state();
        st.client.focus_main();
        // Ctrl+H (Char('h') + CONTROL) moves focus left to the nav sidebar.
        st.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(st.client.focus(), Focus::Sidebar, "Ctrl+H focuses the sidebar");
        // Ctrl+L moves focus right to the main pane.
        st.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(st.client.focus(), Focus::Main, "Ctrl+L focuses the main pane");
    }

    #[test]
    fn focus_move_to_the_pane_that_already_has_focus_is_a_noop() {
        let mut st = test_state();
        st.client.focus_sidebar();
        st.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(st.client.focus(), Focus::Sidebar);
        st.client.focus_main();
        st.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(st.client.focus(), Focus::Main);
    }

    #[test]
    fn ctrl_h_and_ctrl_l_resolve_to_the_focus_actions() {
        let st = test_state();
        assert_eq!(
            st.focus_move_action(&KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL)),
            Some(GlobalAction::FocusSidebar),
        );
        assert_eq!(
            st.focus_move_action(&KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL)),
            Some(GlobalAction::FocusMain),
        );
    }

    #[test]
    fn backspace_is_never_mistaken_for_a_focus_move() {
        // On a terminal without keyboard enhancement Ctrl+H arrives as
        // Backspace; it must not steal focus, and plain Backspace stays a
        // session key. The action only matches the unambiguous Char('h') +
        // CONTROL form, so every Backspace shape resolves to no focus move.
        let st = test_state();
        assert!(
            st.focus_move_action(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
                .is_none(),
            "plain Backspace is not a focus move",
        );
        assert!(
            st.focus_move_action(&KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL))
                .is_none(),
            "Ctrl+Backspace is not Ctrl+H either",
        );
    }

    #[test]
    fn focus_moves_stay_within_an_open_review() {
        // While a review owns the main area the moves step between its panel
        // and content view, never out to the nav sidebar: client focus stays
        // on the main pane (`rt-tui-review`).
        let mut st = test_state();
        st.main_view = MainView::Review("t-1".into());
        st.review = Some(ReviewInterface::new(
            "proj",
            st.connector.clone(),
            "t-1",
            "review-1",
            "/wt",
            "Vim",
            true,
            None,
        ));
        st.client.focus_main();
        // Ctrl+H targets the review panel; client focus stays on main.
        st.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert_eq!(st.client.focus(), Focus::Main, "focus never leaves the review");
        assert!(
            !st.review.as_ref().unwrap().content_focused(),
            "the panel holds focus after Ctrl+H",
        );
        // Ctrl+L targets the content view. With no live content it stays on
        // the panel (the same guard Tab uses) but never leaves the review.
        st.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
        assert_eq!(st.client.focus(), Focus::Main);
    }

    #[test]
    fn show_description_opens_the_task_popover_over_the_live_review() {
        // The review panel's More / `m` yields ReviewAction::ShowDescription,
        // which opens the board's task-detail popover over the main area with
        // the reviewed task's body — and the review (its content session) stays
        // loaded underneath.
        let mut st = test_state();
        st.main_view = MainView::Review("t-1".into());
        let ts = chrono::Utc::now();
        let task = shelbi_state::IssueFile {
            task: shelbi_core::Issue {
                id: "t-1".into(),
                title: "Cold-start cache".into(),
                column: shelbi_core::Column::review(),
                priority: 2,
                assigned_to: None,
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                launch: None,
                created_at: ts,
                updated_at: ts,
                params: Default::default(),
            },
            body: "## Summary\n\nWarm the application cache during startup.".into(),
            tracker_assignees: Vec::new(),
        };
        st.review = Some(ReviewInterface::new(
            "proj",
            st.connector.clone(),
            "t-1",
            "review-1",
            "/wt",
            "Vim",
            true,
            Some(task),
        ));
        assert!(st.overlay.is_none(), "no overlay before More");
        st.apply_review_action(ReviewAction::ShowDescription);
        match &st.overlay {
            Some(ActiveOverlay::TaskDescription { task, .. }) => {
                assert_eq!(task.task.title, "Cold-start cache");
                assert!(task.body.contains("Warm the application cache"));
            }
            _ => panic!("ShowDescription must open the task-description overlay"),
        }
        assert!(st.review.is_some(), "the review stays loaded under the popover");
    }

    #[test]
    fn palette_lists_the_registry_commands_from_the_sidebar_model() {
        // The open palette's entries come from the shelbi-app command registry,
        // built from the shell's sidebar model — so the nav views, the Zen
        // toggle, and the error-log action are all reachable on an empty query.
        // Workspaces are hidden until the user types (they carry
        // `hidden_until_query`), so the empty-query default list leaves them out
        // but a matching query surfaces them.
        let mut st = test_state();
        st.open_palette();
        {
            let Some(ActiveOverlay::Palette(p)) = &st.overlay else {
                panic!("palette should be open");
            };
            let ids: Vec<String> = p.results().into_iter().map(|(e, _)| e.id).collect();
            assert!(ids.contains(&"view:tasks".to_string()), "Issues view reachable");
            assert!(
                ids.contains(&"action:toggle-zen".to_string()),
                "Zen toggle reachable"
            );
            assert!(
                ids.contains(&"action:error-log".to_string()),
                "error log reachable"
            );
            assert!(
                !ids.contains(&"workspace:alpha".to_string()),
                "an empty query must not list workspaces"
            );
        }

        // Typing a query that matches the workspace surfaces it in the results.
        {
            let Some(ActiveOverlay::Palette(p)) = &mut st.overlay else {
                panic!("palette should be open");
            };
            for c in "alp".chars() {
                p.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), None);
            }
            let ids: Vec<String> = p.results().into_iter().map(|(e, _)| e.id).collect();
            assert!(
                ids.contains(&"workspace:alpha".to_string()),
                "typing a matching query surfaces the workspace, got: {ids:?}"
            );
        }
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
        // Enter opens the selected native view and moves focus to the main area.
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Native(View::Activity)));
        assert_eq!(st.client.focus(), Focus::Main);
    }

    #[test]
    fn sidebar_j_and_k_move_the_selection_without_opening_or_focusing_main() {
        // j/k mirror Down/Up: they move the selection only. Focus stays on the
        // sidebar and the main view is untouched until the row is opened.
        let mut st = test_state();
        // Open Issues so there is a concrete main view to watch, then step back
        // to the sidebar to navigate.
        st.show(RowTarget::Native(View::Issues));
        st.client.focus_sidebar();
        assert_eq!(st.selection(), 0, "starts on Chat");
        st.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(st.selection(), 1, "j → Issues");
        st.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(st.selection(), 2, "j → Activity");
        st.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert_eq!(st.selection(), 1, "k → Issues");
        assert_eq!(
            st.client.focus(),
            Focus::Sidebar,
            "navigation keeps focus on the sidebar",
        );
        assert!(
            matches!(st.main_view, MainView::Native(View::Issues)),
            "navigation does not change what the main area shows",
        );
    }

    #[test]
    fn space_opens_the_selected_row_and_focuses_main() {
        // Space is an open key, exactly like Enter: it opens the selected row
        // and moves focus to the main area.
        let mut st = test_state();
        st.client.focus_sidebar();
        st.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)); // Issues
        st.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Native(View::Issues)));
        assert_eq!(st.client.focus(), Focus::Main, "Space opens and focuses main");

        // Space on a session row attaches its session and also focuses main.
        // Opening focused main, so step back to the sidebar to navigate.
        st.client.focus_sidebar();
        st.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // Activity
        st.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // alpha
        st.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Session));
        assert_eq!(st.client.focus(), Focus::Main);
    }

    #[test]
    fn issues_and_activity_open_in_the_main_area_from_the_sidebar() {
        // Machines moved to the palette; the sidebar nav is Chat / Issues /
        // Activity only (parity with main). The three-item nav is what
        // `test_state` already carries.
        let mut st = test_state();
        st.client.focus_sidebar();
        // Rows: 0 Chat, 1 Issues, 2 Activity, 3 alpha.
        st.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // Issues
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Native(View::Issues)));
        assert_eq!(st.client.view(), &View::Issues, "the view is recorded for the project");

        // Opening moved focus to the main area, so step back to the sidebar
        // before navigating again (Ctrl+L→sidebar is the user's way back).
        st.client.focus_sidebar();
        st.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)); // Activity
        st.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(st.main_view, MainView::Native(View::Activity)));

        // No sidebar row routes to the Machines view any more.
        let view = st.sidebar_view().expect("sidebar built");
        assert!(
            (0..view.selectable_count())
                .filter_map(|i| view.target_at(i))
                .all(|t| t != RowTarget::Native(View::Machines)),
            "the Machines view is not reachable from the sidebar nav"
        );
    }

    #[test]
    fn machines_opens_from_the_command_palette() {
        // Requirement: Ctrl+P → Machines opens the Machines view even though it
        // has no sidebar nav row.
        let mut st = test_state();
        st.open_palette();
        let entry = {
            let Some(ActiveOverlay::Palette(p)) = &st.overlay else {
                panic!("palette should be open");
            };
            p.results()
                .into_iter()
                .map(|(e, _)| e)
                .find(|e| e.id == "view:machines")
                .expect("palette lists a Machines command")
        };
        st.run_entry(&entry);
        assert!(
            matches!(st.main_view, MainView::Native(View::Machines)),
            "running the palette's Machines command shows the Machines view"
        );
        assert_eq!(st.client.view(), &View::Machines);
    }

    #[test]
    fn the_palettes_reject_entry_asks_for_a_reason() {
        // The registry's `action:reject-review:<id>` carries no reason, so the
        // shell opens the reject prompt for that task instead of sending it.
        let mut st = test_state();
        st.run_entry(&shelbi_palette::Entry {
            id: "action:reject-review:T-7".into(),
            label: "Reject Fix it".into(),
            kind: shelbi_palette::EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: true,
        });
        assert!(
            matches!(&st.overlay, Some(ActiveOverlay::RejectReason { task_id, .. }) if task_id == "T-7"),
            "the reject prompt opens for the task"
        );
    }

    #[test]
    fn restoring_the_machines_view_works_and_the_nav_has_no_machines_row() {
        // A persisted `machines` view (ClientState) restores into the main area,
        // and because Machines is palette-only the sidebar nav highlights no row
        // as the Machines view.
        let mut st = test_state();
        st.apply_restored_view(View::Machines);
        assert!(
            matches!(st.main_view, MainView::Native(View::Machines)),
            "the remembered Machines view restores into the main area"
        );
        let view = st.sidebar_view().expect("sidebar built");
        let nav_targets: Vec<_> = (0..view.selectable_count())
            .filter_map(|i| view.target_at(i))
            .collect();
        assert!(
            !nav_targets.contains(&RowTarget::Native(View::Machines)),
            "no sidebar row routes to the Machines view, got: {nav_targets:?}"
        );
    }

    #[test]
    fn clicking_a_nav_item_opens_it_and_moves_focus_to_main() {
        // The live regression: a mouse click on a nav row opened the view but
        // left keyboard focus on the sidebar, so the next keystroke kept driving
        // the sidebar instead of the opened view. Drive the real mouse event path
        // (`handle_event` → `handle_mouse` → `handle_sidebar_mouse`) at the row's
        // rendered coordinates, then confirm a following key reaches the main
        // view — the exact sequence unit tests calling `handle_key` never
        // exercised.
        let mut st = test_state();
        // Lay the shell out as a draw would: a 28-col sidebar beside a 92-col
        // main area, 40 rows tall (`handle_mouse` routes on these rects, and the
        // sidebar hit map is derived from the sidebar rect).
        st.sidebar_rect = Rect::new(0, 0, 28, 40);
        st.main_rect = Rect::new(28, 0, 92, 40);
        // The user has tabbed into the nav, so focus starts on the sidebar.
        st.client.focus_sidebar();

        // The nav block renders two-row bleed pills below the 6-row header: the
        // label for nav item `k` sits on row `6 + 2k + 1`. Rows are Orchestrator
        // (k=0 → row 7), Issues (k=1 → row 9), Activity (k=2 → row 11). Click the
        // Issues label.
        st.handle_event(Event::Mouse(left_click(5, 9)));
        assert!(
            matches!(st.main_view, MainView::Native(View::Issues)),
            "the click opened the Issues view",
        );
        assert_eq!(
            st.client.focus(),
            Focus::Main,
            "the click moved keyboard focus to the main area",
        );

        // The next keystroke must drive the main view, not the sidebar: a
        // sidebar-nav key (Down) must leave the sidebar selection untouched.
        let sel_before = st.selection();
        st.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
        assert_eq!(
            st.selection(),
            sel_before,
            "with focus on main, a nav key no longer moves the sidebar selection",
        );
    }

    /// A synthetic left-click at `(col, row)` in absolute terminal coordinates.
    fn left_click(col: u16, row: u16) -> crossterm::event::MouseEvent {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A synthetic left-button drag to `(col, row)` (button held, moving).
    fn drag_left(col: u16, row: u16) -> crossterm::event::MouseEvent {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A synthetic left-button release at `(col, row)`.
    fn up_left(col: u16, row: u16) -> crossterm::event::MouseEvent {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// A temp `SHELBI_HOME` for a test that reads or writes global state;
    /// unique per call so parallel tests don't collide (callers still hold
    /// `ENV_LOCK` around the env mutation).
    fn temp_home(tag: &str) -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!(
            "shelbi-shell-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn full_area_spans_the_sidebar_and_main_pane() {
        // The overlay-centering rect is the union of the two columns, so a
        // popover centers across the whole window rather than only the main
        // area. Sidebar [0,28), main [28,120): the union is the full 120-wide
        // window, and its center is well left of the main area's center.
        let mut st = test_state();
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        st.main_rect = Rect::new(28, 0, 92, 20);
        let full = st.full_area();
        assert_eq!(full, Rect::new(0, 0, 120, 20));
        let full_center = full.x + full.width / 2;
        let main_center = st.main_rect.x + st.main_rect.width / 2;
        assert!(
            full_center < main_center,
            "the full-window center ({full_center}) must sit left of the main-area center ({main_center})"
        );
    }

    #[test]
    fn dragging_the_divider_resizes_the_sidebar_and_persists() {
        // A press on the divider, drags, then a release: the width tracks the
        // pointer (clamped to [24, half the window]) and the released value is
        // saved to `~/.shelbi/state.json`.
        let _g = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = temp_home("divider-drag");
        std::env::set_var("SHELBI_HOME", &home);

        let mut st = test_state();
        // A 120-wide window: sidebar [0,28), main [28,120). Divider col = 27.
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        st.main_rect = Rect::new(28, 0, 92, 20);
        assert_eq!(st.divider_col(), 27);

        // Press on the divider starts a drag (and selects nothing).
        let selection_before = st.selection();
        st.handle_mouse(left_click(27, 5));
        assert!(st.sidebar_dragging, "a press on the divider begins a drag");
        assert_eq!(st.selection(), selection_before, "the press selected no row");

        // Drag right: the sidebar's right edge follows the pointer.
        st.handle_mouse(drag_left(40, 5));
        assert_eq!(st.client.sidebar_width(), 41, "width tracks the pointer (col+1)");

        // Drag far left: clamped up to the 24-column minimum.
        st.handle_mouse(drag_left(2, 5));
        assert_eq!(st.client.sidebar_width(), 24, "clamped to the minimum");

        // Drag far right: clamped down to half the window (60).
        st.handle_mouse(drag_left(100, 5));
        assert_eq!(st.client.sidebar_width(), 60, "clamped to half the window");

        // Settle on a mid value and release — the width persists.
        st.handle_mouse(drag_left(37, 5));
        assert_eq!(st.client.sidebar_width(), 38);
        st.handle_mouse(up_left(37, 5));
        assert!(!st.sidebar_dragging, "release ends the drag");
        assert_eq!(st.client.sidebar_width(), 38);
        assert_eq!(
            shelbi_state::sidebar_width().unwrap(),
            Some(38),
            "the released width is saved under ~/.shelbi/"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn dragging_the_divider_while_a_review_is_open_resizes_the_panel() {
        // While a review is open the panel sits in the sidebar's column, so the
        // divider between panel and content is still draggable — and the divider
        // press-drag-release takes priority over the review's mouse routing: the
        // press begins a drag instead of reaching the panel or the content view
        // (`rt-review-screen-hangs-on-connecting`).
        let _g = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = temp_home("review-divider-drag");
        std::env::set_var("SHELBI_HOME", &home);

        let mut st = test_state();
        // A review is open: the panel occupies the sidebar column, the content
        // view the main area.
        st.main_view = MainView::Review("t-1".into());
        st.review = Some(ReviewInterface::new(
            "proj",
            Arc::new(NoopConnector),
            "t-1",
            "rev-1",
            "/tmp/wt",
            "vim",
            false,
            None,
        ));
        // A 120-wide window: panel [0,28), content [28,120). Divider col = 27.
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        st.main_rect = Rect::new(28, 0, 92, 20);
        assert_eq!(st.divider_col(), 27);
        let focus_before = st.client.focus();

        // Press on the divider begins a drag — it does not reach the review
        // (which, on a left press, would focus the main/content pane).
        st.handle_mouse(left_click(27, 5));
        assert!(st.sidebar_dragging, "a divider press begins a drag even over a review");
        assert!(
            matches!(st.main_view, MainView::Review(_)),
            "the review stays open"
        );
        assert_eq!(
            st.client.focus(),
            focus_before,
            "the divider press did not reach the review's content/panel routing"
        );

        // Drag right: the panel's right edge follows the pointer.
        st.handle_mouse(drag_left(40, 5));
        assert_eq!(st.client.sidebar_width(), 41, "the panel width tracks the pointer");

        // Release persists the chosen width and ends the drag; the review is
        // still open.
        st.handle_mouse(up_left(40, 5));
        assert!(!st.sidebar_dragging, "release ends the drag");
        assert_eq!(st.client.sidebar_width(), 41);
        assert!(
            matches!(st.main_view, MainView::Review(_)),
            "the review is still open after the resize"
        );
        assert_eq!(
            shelbi_state::sidebar_width().unwrap(),
            Some(41),
            "the released panel width is saved under ~/.shelbi/"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_click_on_the_divider_opens_no_row_and_no_main_click() {
        // Press+release on the divider with no movement: it begins and ends a
        // (zero-delta) drag — it must not select a sidebar row or open a view.
        let _g = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = temp_home("divider-click");
        std::env::set_var("SHELBI_HOME", &home);

        let mut st = test_state();
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        st.main_rect = Rect::new(28, 0, 92, 20);
        // Start on a session view so an accidental row-open would be visible.
        st.show(RowTarget::Session(SessionRef::Orchestrator));
        let selection_before = st.selection();

        st.handle_mouse(left_click(27, 5));
        st.handle_mouse(up_left(27, 5));
        assert_eq!(st.selection(), selection_before, "no sidebar row was selected");
        assert!(
            matches!(st.main_view, MainView::Session),
            "the divider click opened no new view"
        );
        // The width is unchanged (col 27 → 28, the resting width).
        assert_eq!(st.client.sidebar_width(), 28);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_click_on_the_search_box_opens_the_palette() {
        // The search box sits on header rows 3–5; clicking it opens the command
        // palette, the same as the palette chord.
        let mut st = test_state();
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        st.main_rect = Rect::new(28, 0, 92, 20);
        st.show(RowTarget::Session(SessionRef::Orchestrator));
        assert!(st.overlay.is_none(), "no overlay before the click");

        // Header layout: blank (row 0), title (row 1), blank (row 2), search
        // box (rows 3–5).
        st.handle_mouse(left_click(3, 4));
        assert!(
            matches!(st.overlay, Some(ActiveOverlay::Palette(_))),
            "clicking the search box opens the command palette"
        );
    }

    // --- drag-handle line ---------------------------------------------------

    /// A synthetic pointer-motion (no button) at `(col, row)`.
    fn moved(col: u16, row: u16) -> crossterm::event::MouseEvent {
        use crossterm::event::{MouseEvent, MouseEventKind};
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Render the sidebar exactly as `draw` layers it: content into the area
    /// minus its last column, then the drag-handle line down that last column.
    fn render_sidebar_and_divider(
        st: &ShellState,
        area: Rect,
        active: bool,
    ) -> ratatui::buffer::Buffer {
        let mut buf = ratatui::buffer::Buffer::empty(area);
        let content = Rect {
            width: area.width.saturating_sub(1),
            ..area
        };
        if let Some(view) = st.sidebar_view() {
            view.render(&mut buf, content, 0, true, &st.sidebar_chrome());
        }
        render_divider(&mut buf, area, active);
        buf
    }

    /// The line is a full-height `▏` in the drag column (the sidebar's last
    /// column, == `divider_col`) at two different sidebar widths.
    #[test]
    fn divider_line_fills_the_drag_column_at_two_widths() {
        let st = test_state();
        for width in [24u16, 50u16] {
            let area = Rect::new(0, 0, width, 20);
            let buf = render_sidebar_and_divider(&st, area, false);
            let x = width - 1; // the drag column: sidebar_rect.right() - 1
            for y in 0..area.height {
                assert_eq!(
                    buf[(x, y)].symbol(),
                    crate::theme::DIVIDER_GLYPH,
                    "width {width}: a `▏` fills the drag column at row {y}"
                );
            }
        }
    }

    /// The drawn column matches the column a drag actually starts from.
    #[test]
    fn divider_line_column_matches_the_drag_hit_column() {
        let mut st = test_state();
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        let drag_col = st.divider_col();
        let buf = render_sidebar_and_divider(&st, st.sidebar_rect, false);
        assert!(st.on_divider(drag_col, 5), "the drag column is the hit column");
        assert_eq!(
            buf[(drag_col, 0)].symbol(),
            crate::theme::DIVIDER_GLYPH,
            "the line is drawn in the very column a drag begins from"
        );
    }

    /// At rest the line is dim; while active (hover or drag) it brightens to the
    /// accent and goes bold.
    #[test]
    fn divider_line_dims_at_rest_and_highlights_when_active() {
        let st = test_state();
        let area = Rect::new(0, 0, 28, 20);
        let x = area.width - 1;

        let dim = render_sidebar_and_divider(&st, area, false);
        assert_eq!(dim[(x, 3)].fg, palette().divider, "dim at rest");
        assert!(
            !dim[(x, 3)].modifier.contains(ratatui::style::Modifier::BOLD),
            "the resting line is not bold"
        );

        let hot = render_sidebar_and_divider(&st, area, true);
        assert_eq!(
            hot[(x, 3)].fg,
            palette().divider_active,
            "accent color while active"
        );
        assert!(
            hot[(x, 3)].modifier.contains(ratatui::style::Modifier::BOLD),
            "the active line is bold"
        );
    }

    /// Sidebar content stops one column short of the drag column, so it never
    /// writes into — or gets clipped by — the line. Checked at the 24-column
    /// minimum and a wide sidebar, with a right-aligned `idle` state that would
    /// otherwise reach the edge.
    #[test]
    fn sidebar_content_never_touches_the_drag_column() {
        let st = test_state(); // seeds one idle "alpha" workspace
        for width in [24u16, 48u16] {
            let area = Rect::new(0, 0, width, 20);
            let content = Rect {
                width: width - 1,
                ..area
            };
            // Render content ONLY (no divider), so we can see whether it reaches
            // the drag column on its own.
            let mut buf = ratatui::buffer::Buffer::empty(area);
            let view = st.sidebar_view().unwrap();
            view.render(&mut buf, content, 0, true, &st.sidebar_chrome());

            let drag_col = width - 1;
            for y in 0..area.height {
                let cell = &buf[(drag_col, y)];
                assert!(
                    cell.symbol() == " " || cell.symbol().is_empty(),
                    "width {width}: content left the drag column clear at row {y}, got {:?}",
                    cell.symbol()
                );
            }
            // The right-aligned `idle` state still renders in full within the
            // narrowed content — it is not clipped by reserving the column.
            let rows: Vec<String> = (0..area.height)
                .map(|y| {
                    (0..content.width)
                        .map(|x| buf[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect();
            assert!(
                rows.iter().any(|r| r.contains("idle")),
                "width {width}: the idle state renders unclipped, got:\n{}",
                rows.join("\n")
            );
        }
    }

    /// Pointer motion onto the drag column lights the hover state (and repaints);
    /// motion off it clears it again. An in-progress drag also counts as active.
    #[test]
    fn divider_hover_follows_the_pointer_and_feeds_the_highlight() {
        let mut st = test_state();
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        st.main_rect = Rect::new(28, 0, 92, 20);
        let col = st.divider_col();

        assert!(!st.divider_hover, "no hover before any motion");

        st.dirty = false;
        st.handle_mouse(moved(col, 5));
        assert!(st.divider_hover, "motion onto the drag column sets hover");
        assert!(st.dirty, "a hover flip repaints");

        // Motion that stays on the column does not re-flip / re-dirty.
        st.dirty = false;
        st.handle_mouse(moved(col, 7));
        assert!(st.divider_hover, "still hovering");
        assert!(!st.dirty, "no repaint while the hover state is unchanged");

        // Motion off the column clears it.
        st.dirty = false;
        st.handle_mouse(moved(col - 3, 5));
        assert!(!st.divider_hover, "motion off the column clears hover");
        assert!(st.dirty, "clearing the hover repaints");

        // A drag is active even with no hover — the highlight tracks either.
        st.divider_hover = false;
        st.sidebar_dragging = true;
        assert!(
            st.sidebar_dragging || st.divider_hover,
            "a drag in progress keeps the handle highlighted"
        );
    }

    #[test]
    fn a_saved_sidebar_width_restores_on_construction() {
        // A width saved under ~/.shelbi/ is seeded into a freshly-built shell —
        // the dragged width survives quitting and reopening shelbi.
        let _g = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = temp_home("saved-width");
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::set_sidebar_width(33).unwrap();
        let caps = Caps { kitty: true, truecolor: true, nested: None };
        let st = ShellState::new("proj", Arc::new(NoopConnector), caps);
        assert_eq!(
            st.client.sidebar_width(),
            33,
            "the saved width is restored on construction"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// A synthetic wheel event (`up` scrolls up, else down) over `(col, row)`.
    fn wheel(up: bool, col: u16, row: u16) -> crossterm::event::MouseEvent {
        use crossterm::event::{MouseEvent, MouseEventKind};
        MouseEvent {
            kind: if up {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            },
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn clicking_a_native_view_row_selects_and_opens_it() {
        // rt-tui-sidebar-click-activates: a single click opens the row, the same
        // as Enter — no second keystroke needed.
        let mut st = test_state();
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        st.client.focus_main();
        // Sidebar geometry: rows 0-5 are the header (title, margin, the 2-row
        // search box across rows 2-4, margin), then the nav block interleaves
        // items with separator lines — Chat on y=7, Issues on y=9, Activity on
        // y=11 (even rows between are inert separators). Click Issues.
        st.handle_mouse(left_click(2, 9));
        assert_eq!(st.selection(), 1, "selection moved to Issues");
        assert!(
            matches!(st.main_view, MainView::Native(View::Issues)),
            "the click opened the Issues view"
        );
        // Clicking a row moves focus to the main area, for a native view the
        // same as a session (so the click lands the user on what they opened).
        assert_eq!(st.client.focus(), Focus::Main);

        // Clicking Activity (y=11) switches the main area again and keeps main
        // focus.
        st.handle_mouse(left_click(2, 11));
        assert_eq!(st.selection(), 2, "selection moved to Activity");
        assert!(matches!(st.main_view, MainView::Native(View::Activity)));
        assert_eq!(st.client.focus(), Focus::Main);
    }

    #[test]
    fn clicking_a_workspace_row_attaches_its_session() {
        let mut st = test_state();
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        st.client.focus_sidebar();
        // After the nav block (ends y=12) comes a blank (y=13), the
        // "— Workspaces —" header (y=14), then the single flat workspace alpha
        // on y=15 (selectable ordinal 3).
        st.handle_mouse(left_click(2, 15));
        assert_eq!(st.selection(), 3, "selection moved to alpha");
        assert!(
            matches!(st.main_view, MainView::Session),
            "the click opened a session in the main area"
        );
        assert_eq!(
            st.sessions.current_target(),
            Some(&SessionRef::Workspace("alpha".into())),
            "the clicked workspace's session is attached"
        );
        // A session click takes main focus, exactly as Enter does.
        assert_eq!(st.client.focus(), Focus::Main);
    }

    /// Poll the main-area session manager until its in-flight connect settles,
    /// so a test can assert the resolved [`MainState`].
    fn settle_sessions(st: &mut ShellState) {
        for _ in 0..400 {
            if st.sessions.poll() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the session connect never settled");
    }

    #[test]
    fn opening_an_idle_workspace_shows_its_placeholder() {
        // AC: opening (Enter/click) an idle workspace with no session shows the
        // idle-workspace placeholder in the main area, and the main view agrees
        // with the sidebar selection (`rt-tui-idle-workspace-open`).
        let mut st = test_state_with_connector(Arc::new(IdleWorkspaceConnector {
            machine: "hub".into(),
            branch: Some("main".into()),
        }));
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        st.client.focus_sidebar();
        // The single flat workspace `alpha` sits on y=15 (see the click test).
        st.handle_mouse(left_click(2, 15));
        assert!(matches!(st.main_view, MainView::Session));
        assert_eq!(
            st.sessions.current_target(),
            Some(&SessionRef::Workspace("alpha".into())),
        );
        settle_sessions(&mut st);
        match st.sessions.state() {
            MainState::Idle(info) => {
                assert_eq!(info.name, "alpha");
                assert_eq!(info.machine, "hub");
                assert_eq!(info.branch.as_deref(), Some("main"));
            }
            _ => panic!("an idle workspace lands in MainState::Idle"),
        }
    }

    #[test]
    fn idle_placeholder_lines_carry_identity_and_a_start_hint() {
        // Full identity: name, then "machine · branch", a blank, the idle line,
        // and the start hint.
        let lines = idle_placeholder_lines(
            &session::IdleInfo {
                name: "vector".into(),
                machine: "hub".into(),
                branch: Some("jlong/widget".into()),
            },
            "Ctrl+Space",
        );
        assert_eq!(lines[0], "vector");
        assert_eq!(lines[1], "hub · jlong/widget");
        assert!(lines.iter().any(|l| l.contains("Idle")));
        assert!(lines.iter().any(|l| l.contains("Issues board")));
        // The start hint shows the actual palette chord, not a hard-coded one.
        assert!(
            lines.iter().any(|l| l.contains("Ctrl+Space")),
            "the hint names the configured palette chord: {lines:?}"
        );

        // A different chord flows through verbatim.
        let custom = idle_placeholder_lines(
            &session::IdleInfo {
                name: "vector".into(),
                machine: "hub".into(),
                branch: None,
            },
            "⌃P",
        );
        assert!(custom.iter().any(|l| l.contains("⌃P")));

        // A remote workspace (no resolved branch) drops the branch part.
        let remote = idle_placeholder_lines(
            &session::IdleInfo {
                name: "vector".into(),
                machine: "gpu-box".into(),
                branch: None,
            },
            "Ctrl+Space",
        );
        assert_eq!(remote[1], "gpu-box");

        // A workspace whose config couldn't load still shows its name.
        let bare = idle_placeholder_lines(
            &session::IdleInfo {
                name: "vector".into(),
                machine: String::new(),
                branch: None,
            },
            "Ctrl+Space",
        );
        assert_eq!(bare[0], "vector");
        assert!(bare.iter().any(|l| l.contains("Idle")));
    }

    #[test]
    fn a_change_for_the_shown_idle_workspace_retries_the_attach() {
        // AC: after a session for the shown workspace starts, the main area
        // attaches without re-selecting. A workspace change naming the shown
        // (idle) workspace kicks a retry; an unrelated one does not
        // (`rt-tui-idle-workspace-open`).
        let mut st = test_state_with_connector(Arc::new(IdleWorkspaceConnector {
            machine: "hub".into(),
            branch: None,
        }));
        st.show(RowTarget::Session(SessionRef::Workspace("alpha".into())));
        settle_sessions(&mut st);
        assert!(matches!(st.sessions.state(), MainState::Idle(_)));

        // A change for a different workspace is ignored.
        assert!(!st.retry_shown_workspace(&["beta".into()]));
        assert!(matches!(st.sessions.state(), MainState::Idle(_)));

        // A change naming the shown workspace retries: it goes back to
        // connecting (the worker re-runs) with no re-selection.
        assert!(st.retry_shown_workspace(&["alpha".into()]));
        assert!(matches!(st.sessions.state(), MainState::Connecting(_)));
    }

    #[test]
    fn render_idle_workspace_paints_the_identity_into_the_area() {
        // The placeholder actually paints the workspace identity into the main
        // area's buffer (not just the pure line builder).
        let area = Rect::new(0, 0, 60, 12);
        let mut buf = ratatui::buffer::Buffer::empty(area);
        render_idle_workspace(
            &mut buf,
            area,
            &session::IdleInfo {
                name: "vector".into(),
                machine: "hub".into(),
                branch: Some("main".into()),
            },
            "Ctrl+Space",
        );
        let painted: String = (0..area.height)
            .flat_map(|y| (0..area.width).map(move |x| (x, y)))
            .filter_map(|(x, y)| buf.cell((x, y)).map(|c| c.symbol().to_string()))
            .collect();
        assert!(painted.contains("vector"), "name painted: {painted:?}");
        assert!(painted.contains("hub"), "machine painted");
        assert!(painted.contains("Idle"), "idle line painted");
        // The name is bold white, the body muted (the Figma `workspace-idle`).
        let name_cell = (0..area.width)
            .find_map(|x| buf.cell((x, 0)).filter(|c| c.symbol() == "v"))
            .expect("name row painted");
        assert_eq!(name_cell.fg, Color::White, "name is white");
        assert!(name_cell.modifier.contains(Modifier::BOLD), "name is bold");
    }

    #[test]
    fn main_area_paints_no_opaque_background() {
        // Neither the idle-workspace placeholder nor the "connecting" placeholder
        // paints a full-area fill: unpainted cells stay on the terminal default
        // (`Color::Reset`) so a transparent terminal shows through the main area.
        let area = Rect::new(0, 0, 60, 12);

        let mut idle = ratatui::buffer::Buffer::empty(area);
        render_idle_workspace(
            &mut idle,
            area,
            &session::IdleInfo {
                name: "vector".into(),
                machine: "hub".into(),
                branch: Some("main".into()),
            },
            "Ctrl+Space",
        );
        // The last row is well below the few lines of identity text — a blank
        // main-area cell with no fill.
        let blank_y = area.height - 1;
        for x in 0..area.width {
            assert_eq!(
                idle[(x, blank_y)].bg,
                Color::Reset,
                "idle placeholder leaves the main area transparent, col {x}"
            );
        }

        let mut placeholder = ratatui::buffer::Buffer::empty(area);
        render_placeholder(&mut placeholder, area, "Connecting to alpha…");
        for y in 0..area.height {
            for x in 0..area.width {
                assert_eq!(
                    placeholder[(x, y)].bg,
                    Color::Reset,
                    "placeholder leaves the main area transparent, cell ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn opening_an_idle_workspace_keeps_the_regular_sidebar() {
        // Correction (idle workspaces keep the regular sidebar): opening an idle
        // workspace shows its agent session (→ idle placeholder) under the nav
        // sidebar, never the workspace sidebar.
        let mut st = test_state(); // alpha is idle (current_task None)
        assert!(!st.workspace_has_task("alpha"), "alpha has no task");
        st.show(RowTarget::Session(SessionRef::Workspace("alpha".into())));
        assert!(st.workspace.is_none(), "no workspace interface for an idle slot");
        assert!(st.workspace_opening.is_none(), "no workspace open in flight");
        assert!(
            matches!(st.main_view, MainView::Session),
            "an idle workspace keeps the plain session view"
        );
    }

    #[test]
    fn opening_a_workspace_with_a_task_opens_the_workspace_sidebar() {
        // A workspace with a running task opens the workspace sidebar (resolved
        // off-thread), not the plain session view.
        let mut st = test_state();
        {
            let ws = &mut st.sidebar_model.as_mut().unwrap().workspaces[0];
            ws.current_task = Some("t-1".into());
            ws.agent = Some("developer".into());
            ws.badge = shelbi_app::view::WorkspaceBadge::Working;
        }
        assert!(st.workspace_has_task("alpha"), "alpha now has a task");
        st.show(RowTarget::Session(SessionRef::Workspace("alpha".into())));
        // begin_workspace resolves off-thread; synchronously it marks the open as
        // in flight rather than falling through to the plain session path.
        assert_eq!(
            st.workspace_opening.as_deref(),
            Some("alpha"),
            "a workspace-with-task open goes through begin_workspace"
        );
    }

    #[test]
    fn an_open_workspace_header_firms_up_from_the_fresh_sidebar_model() {
        // Regression (the "IDLE on first open" bug): an open workspace panel's
        // header is re-derived from the live sidebar model on each board refresh
        // — the same source the sidebar badge reads — so a header that opened
        // stale firms up to the real status within a poll tick, and a handoff
        // that idles the workspace is reflected without a reopen.
        let mut st = test_state();
        // Open the panel directly, seeded IDLE (as if the first-open resolve had
        // come back before the task was visible to it).
        st.workspace = Some(WorkspaceInterface::new(
            "proj",
            st.connector.clone(),
            "alpha",
            "/wt/alpha",
            "Vim",
            "Developer",
            default_ws_status(),
            None,
        ));
        assert_eq!(st.workspace.as_ref().unwrap().header_status_text(), "IDLE");

        // A refresh lands with alpha working (current_task set) — the sidebar
        // badge the open gate already trusted.
        let mut sidebar = st.sidebar_model.clone().unwrap();
        sidebar.workspaces[0].current_task = Some("t-1".into());
        sidebar.workspaces[0].badge = shelbi_app::view::WorkspaceBadge::Working;
        st.apply_snapshot(ShellSnapshot {
            sidebar: Some(sidebar),
            board: None,
            activity: None,
            machines: None,
        });
        assert_eq!(
            st.workspace.as_ref().unwrap().header_status_text(),
            "IN PROGRESS",
            "the open header firms up from the fresh sidebar model"
        );

        // When the task hands off (current_task cleared) the workspace idles.
        let mut sidebar = st.sidebar_model.clone().unwrap();
        sidebar.workspaces[0].current_task = None;
        sidebar.workspaces[0].badge = shelbi_app::view::WorkspaceBadge::Idle;
        st.apply_snapshot(ShellSnapshot {
            sidebar: Some(sidebar),
            board: None,
            activity: None,
            machines: None,
        });
        assert_eq!(
            st.workspace.as_ref().unwrap().header_status_text(),
            "IDLE",
            "a handoff that clears the assignment idles the header"
        );
    }

    #[test]
    fn opening_a_workspace_with_a_dead_session_shows_its_last_output() {
        // AC: opening a workspace whose session exited shows its last output
        // line — the orchestrator view's existing behavior, routed through the
        // Failed placeholder (`rt-tui-idle-workspace-open`).
        struct DeadConnector;
        impl session::Connector for DeadConnector {
            fn connect(
                &self,
                _p: &str,
                _t: &SessionRef,
            ) -> Result<session::Connected, session::ConnectFailure> {
                Err(session::ConnectFailure::Message(
                    "no live session `proj/ws/alpha` — last output: zsh: command not found: claude"
                        .into(),
                ))
            }
        }
        let mut st = test_state_with_connector(Arc::new(DeadConnector));
        st.show(RowTarget::Session(SessionRef::Workspace("alpha".into())));
        settle_sessions(&mut st);
        match st.sessions.state() {
            MainState::Failed(_, err) => {
                assert!(err.contains("last output: zsh: command not found: claude"));
            }
            _ => panic!("a dead session lands in MainState::Failed with its last line"),
        }
    }

    #[test]
    fn opening_a_session_drops_an_open_review() {
        // The review interface draws over the main area regardless of
        // `main_view`, so navigating to a workspace/session must drop it — else
        // the idle placeholder (or any session) is hidden behind the review,
        // which is the bug that motivated this task (`rt-tui-idle-workspace-open`).
        let mut st = test_state();
        st.main_view = MainView::Review("t-1".into());
        st.review = Some(ReviewInterface::new(
            "proj",
            Arc::new(NoopConnector),
            "t-1",
            "rev-1",
            "/tmp/wt",
            "vim",
            false,
            None,
        ));
        st.show(RowTarget::Session(SessionRef::Orchestrator));
        assert!(matches!(st.main_view, MainView::Session));
        assert!(st.review.is_none(), "navigating away drops the review interface");
    }

    #[test]
    fn clicking_a_section_header_or_blank_row_opens_nothing() {
        let mut st = test_state();
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        // Open Activity first so we can prove a header/blank click doesn't change it.
        st.show(RowTarget::Native(View::Activity));
        let before = st.selection();
        // y=14 is the "— Workspaces —" section header (the header block is 6 rows).
        st.handle_mouse(left_click(2, 14));
        assert_eq!(st.selection(), before, "a header click doesn't move the selection");
        assert!(
            matches!(st.main_view, MainView::Native(View::Activity)),
            "a header click opens nothing"
        );
        // y=17 is blank space below the last row (alpha, y=15).
        st.handle_mouse(left_click(2, 17));
        assert_eq!(st.selection(), before, "a blank click doesn't move the selection");
        assert!(matches!(st.main_view, MainView::Native(View::Activity)));
    }

    #[test]
    fn wheel_scrolling_moves_the_selection_without_opening() {
        let mut st = test_state();
        st.sidebar_rect = Rect::new(0, 0, 28, 20);
        // Start on a session view so an accidental open would be visible.
        st.show(RowTarget::Session(SessionRef::Orchestrator));
        assert_eq!(st.selection(), 0);
        st.handle_mouse(wheel(false, 2, 2)); // scroll down
        assert_eq!(st.selection(), 1, "wheel down moved the selection");
        assert!(
            matches!(st.main_view, MainView::Session),
            "wheel scrolling opens nothing"
        );
        assert_eq!(
            st.client.focus(),
            Focus::Sidebar,
            "wheel scrolling never moves focus to main",
        );
        st.handle_mouse(wheel(true, 2, 2)); // scroll up
        assert_eq!(st.selection(), 0, "wheel up moved the selection back");
        assert!(matches!(st.main_view, MainView::Session));
    }

    #[test]
    fn the_embedded_board_routes_moves_through_the_executor() {
        // The shell installs the executor-routing move persister so board moves go
        // through the shelbi-app executor (daemon-backed when the setting is on);
        // the standalone process keeps its default direct path (move_persister None).
        let st = test_state();
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
        let mut st = test_state();
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
        use shelbi_app::view::{MachineRow, MachineWorkspace, MachinesModel, SlotAssignment};
        use shelbi_core::MachineKind;

        let mut st = test_state();
        // Feed the machines view one machine with one workspace.
        st.machines.apply_data(MachinesModel {
            display_name: Some("proj".into()),
            machines: vec![MachineRow {
                name: "local".into(),
                kind: MachineKind::Local,
                host: None,
                is_local: true,
                tags: vec![],
                remote: None,
                reachability: shelbi_orchestrator::machine::Reachability::Reachable,
                workspaces: vec![MachineWorkspace {
                    name: "alpha".into(),
                    is_review_slot: false,
                    state: None,
                    current_task: None,
                    assignment: SlotAssignment::Idle,
                    agent: None,
                    task_title: None,
                    branch: None,
                    review: None,
                }],
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

    // --- queued review load (rt-tui-review-load-queued) --------------------

    use crate::overlay::review_confirm::Slot;

    /// A stubbed daemon for the queued-load flow: staged resolve results and a
    /// recorded, canned `load`, so the shell's routing is exercised without a
    /// real daemon.
    #[derive(Default)]
    struct StubReviewBackend {
        /// The next resolve to return (consumed), so a test can stage a Queued
        /// result and then a Serving one.
        resolve: Mutex<Option<Result<ResolvedReview, String>>>,
        load_result: Mutex<Option<Result<(), String>>>,
        load_calls: Mutex<Vec<(String, String, String)>>,
    }

    impl StubReviewBackend {
        fn stage_resolve(&self, r: Result<ResolvedReview, String>) {
            *self.resolve.lock().unwrap() = Some(r);
        }
        fn stage_load(&self, r: Result<(), String>) {
            *self.load_result.lock().unwrap() = Some(r);
        }
    }

    impl ReviewBackend for StubReviewBackend {
        fn resolve(&self, _project: &str, _task: &str) -> Result<ResolvedReview, String> {
            self.resolve
                .lock()
                .unwrap()
                .take()
                .unwrap_or_else(|| Err("no resolve staged".into()))
        }
        fn load(&self, project: &str, task: &str, workspace: &str) -> Result<(), String> {
            self.load_calls
                .lock()
                .unwrap()
                .push((project.into(), task.into(), workspace.into()));
            self.load_result
                .lock()
                .unwrap()
                .take()
                .unwrap_or(Ok(()))
        }
    }

    fn slot(name: &str) -> Slot {
        Slot {
            name: name.into(),
            occupant: None,
        }
    }

    /// Pump the off-thread review job to completion (the stub is instant, so the
    /// bounded wait never spins long).
    fn pump_review(st: &mut ShellState) {
        for _ in 0..1000 {
            if st.poll_review() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("review job never completed");
    }

    #[test]
    fn opening_a_review_focuses_main_up_front_not_only_when_the_async_open_completes() {
        // Opening a review resolves off the UI thread. Focus must move to the
        // main area the instant the row is activated — not wait for the async
        // resolve, which may be slow, fail, or find the review already open.
        // Before the fix `show`'s Review arm called only `begin_review` (async)
        // and relied on `build_review_interface` to focus main on success, so a
        // slow or failed resolve stranded focus on the sidebar — the live
        // regression for review rows.
        let stub = Arc::new(StubReviewBackend::default());
        // A failing resolve: the async path never reaches the builder that would
        // focus main, so only an up-front focus move keeps focus off the sidebar.
        stub.stage_resolve(Err("resolve failed".into()));
        let mut st = test_state();
        st.review_backend = stub.clone();
        st.client.focus_sidebar();

        // Activate the review row exactly as a click / Enter would (`show`).
        st.show(RowTarget::Review("T-1".into()));
        assert_eq!(
            st.client.focus(),
            Focus::Main,
            "activating a review row moves focus to the main area synchronously",
        );

        // Even after the failing async resolve lands, focus stays on main — the
        // failure path never yanks it back to the sidebar.
        pump_review(&mut st);
        assert_eq!(
            st.client.focus(),
            Focus::Main,
            "a failed async resolve leaves focus on the main area",
        );
        assert!(st.review.is_none(), "the failed resolve opened no interface");
    }

    #[test]
    fn enter_on_a_queued_review_opens_the_slot_picker_over_the_free_slots() {
        // AC1: a queued review (not on a slot) resolves to the free review slots
        // and raises the "Load for review" picker over them — not the interface.
        let stub = Arc::new(StubReviewBackend::default());
        stub.stage_resolve(Ok(ResolvedReview::Queued {
            title: "Fix login".into(),
            free_slots: vec![slot("review-1"), slot("review-2")],
            total_slots: 2,
        }));
        let mut st = test_state();
        st.review_backend = stub.clone();
        st.begin_review("T-1".into());
        pump_review(&mut st);
        match &st.overlay {
            Some(ActiveOverlay::ReviewConfirm { task_id, dialog }) => {
                assert_eq!(task_id, "T-1");
                assert!(dialog.has_slots(), "the picker lists the free slots");
                assert!(dialog.is_picker(), "two free slots → a picker");
            }
            other => panic!("expected the slot picker, got {:?}", other.is_some()),
        }
        // The main area has NOT switched to a review (nothing is loaded yet).
        assert!(!matches!(st.main_view, MainView::Review(_)));
        assert!(st.review.is_none());
    }

    #[test]
    fn every_review_slot_busy_reports_and_loads_nothing() {
        // AC3: slots exist but none are free → the overlay reports it (a no-slots
        // informational dialog) and no load is started.
        let stub = Arc::new(StubReviewBackend::default());
        stub.stage_resolve(Ok(ResolvedReview::Queued {
            title: "Fix login".into(),
            free_slots: Vec::new(),
            total_slots: 2,
        }));
        let mut st = test_state();
        st.review_backend = stub.clone();
        st.begin_review("T-2".into());
        pump_review(&mut st);
        match &st.overlay {
            Some(ActiveOverlay::ReviewConfirm { dialog, .. }) => {
                assert!(!dialog.has_slots(), "every slot busy → a no-slots report");
            }
            other => panic!("expected the busy report, got {:?}", other.is_some()),
        }
        // Nothing was loaded, and dismissing the report loads nothing either.
        assert!(st.review_open_pending.is_none());
        st.apply_overlay_event(OverlayEvent::ReviewCancelled);
        assert!(st.overlay.is_none());
        assert!(
            stub.load_calls.lock().unwrap().is_empty(),
            "an all-busy report must never load"
        );
    }

    #[test]
    fn confirming_loads_through_the_daemon_then_review_opened_opens_the_interface() {
        // AC2: confirming a free slot loads the task through the (stubbed) daemon
        // off the UI thread, and the daemon's ReviewOpened event opens the native
        // interface once the slot is serving.
        let stub = Arc::new(StubReviewBackend::default());
        stub.stage_resolve(Ok(ResolvedReview::Queued {
            title: "Fix login".into(),
            free_slots: vec![slot("review-1")],
            total_slots: 1,
        }));
        stub.stage_load(Ok(()));
        let mut st = test_state();
        st.review_backend = stub.clone();

        // Open the picker, then confirm its single slot.
        st.begin_review("T-1".into());
        pump_review(&mut st);
        assert!(matches!(st.overlay, Some(ActiveOverlay::ReviewConfirm { .. })));
        st.apply_overlay_event(OverlayEvent::ReviewConfirmed {
            task_id: "T-1".into(),
            slot: "review-1".into(),
        });
        // The confirm closes the overlay, records the task as pending its serve,
        // and dispatches the load off-thread.
        assert!(st.overlay.is_none());
        assert_eq!(st.review_open_pending.as_deref(), Some("T-1"));
        pump_review(&mut st); // the load job lands
        assert_eq!(
            &*stub.load_calls.lock().unwrap(),
            &[("proj".into(), "T-1".into(), "review-1".into())],
            "the load routes to the daemon for the chosen slot"
        );
        // The load succeeded; still waiting for the serve event (no interface yet).
        assert_eq!(st.review_open_pending.as_deref(), Some("T-1"));
        assert!(st.review.is_none());

        // The daemon reports the slot serving: the interface opens for this task.
        stub.stage_resolve(Ok(ResolvedReview::Serving(ReviewOpenParams {
            task_id: "T-1".into(),
            slot: "review-1".into(),
            worktree: "/wt".into(),
            editor_name: "Vim".into(),
            has_review_url: false,
            task: None,
        })));
        st.apply_layout_event(shelbi_state::LayoutEvent::ReviewOpened {
            workspace: "review-1".into(),
            task: "T-1".into(),
        });
        pump_review(&mut st); // the re-resolve (now serving) lands
        assert!(matches!(st.main_view, MainView::Review(id) if id == "T-1"));
        assert_eq!(st.review.as_ref().map(|r| r.task_id()), Some("T-1"));
        assert!(st.review_open_pending.is_none());
    }

    #[test]
    fn a_background_review_opened_for_another_task_never_steals_the_view() {
        // A ReviewOpened this client did not initiate (another client's load, a
        // poller resume) must not open an interface here — matching the tmux
        // sidebar's no-focus resume.
        let stub = Arc::new(StubReviewBackend::default());
        let mut st = test_state();
        st.review_backend = stub.clone();
        // Not pending anything.
        st.apply_layout_event(shelbi_state::LayoutEvent::ReviewOpened {
            workspace: "review-1".into(),
            task: "someone-elses".into(),
        });
        assert!(st.review.is_none());
        assert!(!matches!(st.main_view, MainView::Review(_)));
        assert!(st.review_rx.is_none(), "no resolve was kicked off");
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
        let mut st = ShellState::new_with(
            "proj",
            Arc::new(NoopConnector),
            caps,
            lifecycle,
            Arc::new(ScriptedBootstrap::default()),
        );
        // Hub-free workers so `switch_project` doesn't touch the socket in tests.
        st.make_workers = recording_workers_factory(Arc::new(Mutex::new(Vec::new())));
        st
    }

    /// A bootstrap that blocks until the test releases its gate, standing in for
    /// a slow daemon/dashboard start (`rt-tui-headless-startup-block`).
    struct BlockingBootstrap {
        started: Arc<std::sync::atomic::AtomicBool>,
        gate: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl Bootstrap for BlockingBootstrap {
        fn bootstrap(&self, _project: &str) -> Result<(), String> {
            self.started
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // Block here until the test opens the gate — a slow bootstrap.
            let _ = self.gate.lock().unwrap().recv();
            Ok(())
        }
    }

    #[test]
    fn a_slow_bootstrap_never_blocks_the_ui_thread() {
        use std::sync::atomic::{AtomicBool, Ordering};
        // The headless-startup bug was the shell drawing nothing until the
        // daemon/dashboard bootstrap finished. Prove the inverse: with the
        // bootstrap wedged, the shell's event-loop calls stay non-blocking and
        // input is still handled (so the first frame — drawn by the loop before
        // this ever completes — is never gated on it).
        let caps = Caps {
            kitty: true,
            truecolor: true,
            nested: None,
        };
        let (open_gate, gate) = std::sync::mpsc::channel();
        let started = Arc::new(AtomicBool::new(false));
        let boot = Arc::new(BlockingBootstrap {
            started: started.clone(),
            gate: Mutex::new(gate),
        });
        let mut st = ShellState::new_with(
            "proj",
            Arc::new(NoopConnector),
            caps,
            Arc::new(RecordingLifecycle::default()),
            boot,
        );

        st.spawn_startup();
        // The bootstrap thread is running (and now wedged on the gate).
        let ran = wait_until(Duration::from_secs(2), || started.load(Ordering::SeqCst));
        assert!(ran, "the bootstrap runs off the UI thread");

        // Polling it does not block, and reports not-yet-done while wedged.
        assert!(!st.poll_startup(), "a pending bootstrap leaves poll_startup non-blocking");

        // The shell is fully interactive meanwhile: Ctrl+Space opens the palette.
        st.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL));
        assert!(
            matches!(st.overlay, Some(ActiveOverlay::Palette(_))),
            "input is handled while the bootstrap is still running"
        );

        // Release the gate; the job completes and poll_startup folds it in.
        open_gate.send(()).unwrap();
        let done = wait_until(Duration::from_secs(2), || st.poll_startup());
        assert!(done, "poll_startup reports completion once the bootstrap returns");
    }

    /// A bootstrap that records every project it was asked to bring up. A project
    /// with a gate registered (see [`ScriptedBootstrap::gate`]) blocks until the
    /// test sends its result; any other returns `Ok(())` at once. Stands in for
    /// the daemon/dashboard bootstrap so a project switch touches no real
    /// orchestrator in tests.
    #[derive(Default)]
    struct ScriptedBootstrap {
        calls: Mutex<Vec<String>>,
        gates: Mutex<std::collections::HashMap<String, Receiver<Result<(), String>>>>,
        /// Projects whose bootstrap has returned (set just before returning).
        finished: Mutex<Vec<String>>,
    }
    impl ScriptedBootstrap {
        /// Gate `project`'s bootstrap; the returned sender releases it with a result.
        fn gate(&self, project: &str) -> std::sync::mpsc::Sender<Result<(), String>> {
            let (tx, rx) = std::sync::mpsc::channel();
            self.gates.lock().unwrap().insert(project.to_string(), rx);
            tx
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn finished(&self, project: &str) -> bool {
            self.finished.lock().unwrap().iter().any(|p| p == project)
        }
    }
    impl Bootstrap for ScriptedBootstrap {
        fn bootstrap(&self, project: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(project.to_string());
            let gate = self.gates.lock().unwrap().remove(project);
            let result = match gate {
                Some(rx) => rx.recv().unwrap_or(Ok(())),
                None => Ok(()),
            };
            self.finished.lock().unwrap().push(project.to_string());
            result
        }
    }

    fn scripted_state(boot: Arc<ScriptedBootstrap>) -> ShellState {
        let caps = Caps {
            kitty: true,
            truecolor: true,
            nested: None,
        };
        let mut st = ShellState::new_with(
            "proj",
            Arc::new(NoopConnector),
            caps,
            Arc::new(RecordingLifecycle::default()),
            boot,
        );
        st.make_workers = recording_workers_factory(Arc::new(Mutex::new(Vec::new())));
        st
    }

    #[test]
    fn switching_projects_bootstraps_the_target_projects_orchestrator() {
        // The bug: only the launch project's orchestrator was ever started, so
        // switching into a project whose orchestrator wasn't running left its
        // orchestrator view empty. A switch must run the same off-thread
        // bootstrap a fresh launch in that project runs.
        let boot = Arc::new(ScriptedBootstrap::default());
        let release_beta = boot.gate("beta");
        let mut st = scripted_state(boot.clone());

        st.switch_project("beta");
        // The switch returned while beta's bootstrap is still wedged on its gate:
        // it runs off the UI thread, and the starting status shows meanwhile.
        let ran = wait_until(Duration::from_secs(2), || boot.calls() == ["beta"]);
        assert!(ran, "a switch bootstraps the target project: {:?}", boot.calls());
        assert_eq!(st.status.as_deref(), Some(STARTUP_STATUS));
        assert!(!st.poll_startup(), "the pending bootstrap never blocks the UI");

        release_beta.send(Ok(())).unwrap();
        let done = wait_until(Duration::from_secs(2), || st.poll_startup());
        assert!(done, "beta's bootstrap result is folded in");
        assert_eq!(st.status, None, "the starting status clears once it is up");

        // Switching to the already-current project is a no-op: no second
        // bootstrap. Switching back bootstraps `proj` (ensure_dashboard is
        // idempotent for a live orchestrator, so this never double-launches).
        st.switch_project("beta");
        st.switch_project("proj");
        let ran = wait_until(Duration::from_secs(2), || boot.calls() == ["beta", "proj"]);
        assert!(ran, "one bootstrap per real switch: {:?}", boot.calls());
    }

    #[test]
    fn a_late_bootstrap_result_for_a_left_project_is_ignored() {
        // A bootstrap for project A that finishes after the user switched to B
        // must not re-attach or put a status into B's view.
        let boot = Arc::new(ScriptedBootstrap::default());
        let release_proj = boot.gate("proj");
        let release_beta = boot.gate("beta");
        let mut st = scripted_state(boot.clone());

        // The launch project's bootstrap is in flight...
        st.status = Some(STARTUP_STATUS.to_string());
        st.spawn_startup();
        assert!(wait_until(Duration::from_secs(2), || boot.calls() == ["proj"]));
        // ...when the user switches to beta (whose bootstrap is also pending).
        st.switch_project("beta");
        assert!(wait_until(Duration::from_secs(2), || boot.calls() == ["proj", "beta"]));

        // proj's bootstrap now fails late. Its result must never surface.
        release_proj.send(Err("proj failed".into())).unwrap();
        assert!(wait_until(Duration::from_secs(2), || boot.finished("proj")));
        for _ in 0..20 {
            assert!(!st.poll_startup(), "a stale result is not folded into beta");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            st.status.as_deref(),
            Some(STARTUP_STATUS),
            "beta's status is untouched by proj's late failure"
        );

        // beta's own result still lands.
        release_beta.send(Err("beta failed".into())).unwrap();
        assert!(wait_until(Duration::from_secs(2), || st.poll_startup()));
        assert_eq!(st.status.as_deref(), Some("beta failed"));
    }

    #[test]
    fn poll_startup_drops_a_job_keyed_to_another_project() {
        // Belt-and-braces for the project key: a finished job whose project isn't
        // the current one is dropped without touching status or the view.
        let boot = Arc::new(ScriptedBootstrap::default());
        let mut st = scripted_state(boot);
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Err("other failed".into())).unwrap();
        st.startup_rx = Some(("other".into(), rx));
        st.status = None;
        assert!(!st.poll_startup());
        assert!(st.startup_rx.is_none(), "the stale job is dropped");
        assert_eq!(st.status, None);
    }

    /// Spin until `f` is true or `timeout` elapses (bounded; never a hard hang).
    fn wait_until(timeout: Duration, mut f: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        f()
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
    fn the_refresh_source_follows_the_current_project_after_a_switch() {
        // AC5: the snapshot refresher (which feeds the sidebar, board, activity
        // and machines views) must be rebuilt for the new project on every switch,
        // not stay pinned to the launch project. A recording factory logs the
        // project each (re)build is asked for, so the log must trace the switches.
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut st = test_state();
        st.make_workers = recording_workers_factory(log.clone());

        // Start the workers for the launch project (as `run_with` does once).
        st.start_project_workers();
        assert_eq!(*log.lock().unwrap(), vec!["proj".to_string()]);

        // Switching rebuilds the refresher for the new project…
        st.switch_project("beta");
        // …and switching back rebuilds it again for the original.
        st.switch_project("proj");
        assert_eq!(
            *log.lock().unwrap(),
            vec!["proj".to_string(), "beta".to_string(), "proj".to_string()],
            "the refresh source must follow the current project across switches"
        );

        // A no-op switch to the already-current project rebuilds nothing.
        st.switch_project("proj");
        assert_eq!(
            log.lock().unwrap().len(),
            3,
            "switching to the current project must not rebuild the workers"
        );
    }

    #[test]
    fn a_switch_discards_a_snapshot_left_in_flight_from_the_old_project() {
        // AC3: a snapshot produced by the old project's refresher but not yet
        // folded in must never be rendered after the switch. The switch drops the
        // old worker bundle (and its snapshot receiver), so `latest_snapshot` only
        // ever sees the new project's refresher.
        let mut st = test_state();
        // Each project's refresher tags its snapshot with the project name.
        st.make_workers = Arc::new(|project: &str| {
            let proj = project.to_string();
            let refresher = refresh::spawn(move |_g| tagged_snapshot(&proj));
            let proj_activity = project.to_string();
            let activity_refresher =
                refresh::spawn(move |_g| ActivityData::tagged_for_test(&proj_activity));
            let closed_refresher = refresh::spawn(|_g| None);
            let (_ctx, change_rx) = std::sync::mpsc::channel();
            let (_ltx, layout_rx) = std::sync::mpsc::channel();
            ProjectWorkers {
                refresher,
                activity_refresher,
                closed_refresher,
                change_rx,
                layout_rx,
                _changes: None,
                _layout_sub: None,
            }
        });

        // Start proj's workers (requests a refresh) but do NOT drain it: proj's
        // snapshot is now in flight. Switching drops proj's bundle — and with it
        // that snapshot — and builds beta's.
        st.start_project_workers();
        st.switch_project("beta");

        // Bounded wait (no fixed sleep) for a snapshot to become available; it can
        // only come from beta's refresher, never proj's dropped one.
        let deadline = Instant::now() + Duration::from_secs(2);
        let snap = loop {
            if let Some(s) = st.latest_snapshot() {
                break s;
            }
            assert!(
                Instant::now() < deadline,
                "the new project's snapshot never arrived"
            );
            std::thread::yield_now();
        };
        assert_eq!(
            snap.sidebar.expect("tagged sidebar").project_label,
            "beta",
            "after the switch latest_snapshot yields only the new project's data; \
             the in-flight proj snapshot was discarded"
        );
    }

    #[test]
    fn the_activity_source_follows_the_current_project_after_a_switch() {
        // Rework (#1631 landed a second, activity-only refresher): the activity
        // feed now also lives in the per-project worker bundle, so after a switch
        // its data must come from the new project — not stay pinned to the launch
        // one. The recording factory's activity refresher tags each read with the
        // project it was built for, so `latest_activity` traces the switches.
        let mut st = test_state();
        let log = Arc::new(Mutex::new(Vec::new()));
        st.make_workers = recording_workers_factory(log);

        st.start_project_workers();
        assert_eq!(
            wait_for_activity_tag(&st).as_deref(),
            Some("proj"),
            "the launch project's activity data"
        );

        st.switch_project("beta");
        assert_eq!(
            wait_for_activity_tag(&st).as_deref(),
            Some("beta"),
            "after the switch the activity data comes from the new project"
        );

        st.switch_project("proj");
        assert_eq!(
            wait_for_activity_tag(&st).as_deref(),
            Some("proj"),
            "switching back follows the original project again"
        );
    }

    /// Bounded wait (no fixed sleep) for the current project's activity refresher
    /// to publish its first read, returning its [`ActivityData::tag_for_test`] tag.
    fn wait_for_activity_tag(st: &ShellState) -> Option<String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(a) = st.latest_activity() {
                return a.tag_for_test().map(str::to_string);
            }
            assert!(
                Instant::now() < deadline,
                "the activity refresher never published"
            );
            std::thread::yield_now();
        }
    }

    #[test]
    fn switch_rebuilds_the_embedded_apps_for_the_new_project() {
        // The embedded board / activity / machines apps carry the project their
        // card moves, `load more`, and workspace-open actions target. Before this
        // they stayed pinned to the launch project after a switch; the rebuild
        // moves every one onto the switched-to project.
        let mut st = test_state();
        assert_eq!(st.kanban.project_name, "proj");
        assert_eq!(st.activity.project_name, "proj");
        assert_eq!(st.machines.project_name, "proj");

        st.switch_project("beta");
        assert_eq!(st.kanban.project_name, "beta");
        assert_eq!(st.activity.project_name, "beta");
        assert_eq!(st.machines.project_name, "beta");
    }

    #[test]
    fn a_cold_switch_paints_a_skeleton_sidebar_for_the_target_project() {
        // AC1/AC4: switching into a not-yet-visited project shows the target
        // project's chrome at once — its nav rows plus a board-loading placeholder
        // — never the previous project's rows, and before any off-thread read.
        let mut st = test_state();
        // The launch project's model is populated (test_state applies one).
        assert!(st
            .sidebar_model
            .as_ref()
            .is_some_and(|m| !m.workspaces.is_empty()));

        st.switch_project("beta");
        let model = st.sidebar_model.as_ref().expect("a skeleton model");
        assert_eq!(model.project_label, "beta", "the target project's chrome");
        assert!(model.board_loading, "a visible loading state while data loads");
        assert!(
            model.workspaces.is_empty(),
            "no workspaces are shown from the previous (or any) project yet"
        );
        assert!(!model.nav.is_empty(), "the nav rows paint immediately");
    }

    #[test]
    fn a_warm_switch_restores_the_cached_sidebar_immediately() {
        // AC2: switching back to an already-visited project is visually immediate —
        // the sidebar it had is re-applied on the switch, not a loading skeleton,
        // so there is no flash while the first refresh runs.
        let mut st = test_state();
        // `proj` has a populated model (from test_state). Leaving it stashes that
        // model; returning restores it.
        let proj_workspaces = st
            .sidebar_model
            .as_ref()
            .expect("launch model")
            .workspaces
            .len();
        assert!(proj_workspaces > 0);

        st.switch_project("beta"); // stashes proj's model; beta is a skeleton
        assert!(st.sidebar_model.as_ref().unwrap().board_loading);

        st.switch_project("proj"); // back to proj: restored from cache, not skeleton
        let restored = st.sidebar_model.as_ref().expect("restored model");
        assert_eq!(restored.project_label, "proj");
        assert!(
            !restored.board_loading,
            "the cached model is applied, not a loading skeleton"
        );
        assert_eq!(
            restored.workspaces.len(),
            proj_workspaces,
            "the cached sidebar's workspaces are back at once"
        );
    }

    /// A config-only skeleton for `beta` as the prefetch would build it: its
    /// display label and declared workspace rows, each awaiting its state.
    fn beta_config_skeleton() -> SidebarModel {
        let mut m = SidebarModel::skeleton("beta");
        m.project_label = "Beta Board".into();
        m.board_loading = false;
        m.workspaces = ["gamma", "omega"]
            .iter()
            .map(|n| WorkspaceRow {
                name: (*n).into(),
                machine: "hub".into(),
                is_remote: false,
                current_task: None,
                agent: None,
                badge: shelbi_app::view::WorkspaceBadge::Loading,
            })
            .collect();
        m
    }

    #[test]
    fn a_cold_switch_paints_the_prefetched_config_skeleton_with_workspace_rows() {
        // A cold switch (no cached model) paints the target's prefetched config
        // skeleton: its label and workspace rows, each with the loading badge
        // instead of a claimed state, never the previous project's rows.
        let mut st = test_state();
        st.skeletons
            .lock()
            .unwrap()
            .insert("beta".into(), beta_config_skeleton());

        st.switch_project("beta");
        let model = st.sidebar_model.as_ref().expect("a skeleton model");
        assert_eq!(model.project_label, "Beta Board");
        let names: Vec<_> = model.workspaces.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["gamma", "omega"], "the target's declared workspaces");

        let buf = render_sidebar_and_divider(&st, Rect::new(0, 0, 40, 30), false);
        let text: String = buf.content.iter().map(|c| c.symbol()).collect();
        assert!(text.contains("Beta Board") && text.contains("gamma"), "{text}");
        assert!(text.contains('…'), "rows show the loading badge: {text}");
        assert!(!text.contains("idle"), "a loading row claims no state: {text}");
        assert!(!text.contains("alpha"), "the previous project's rows are gone: {text}");
    }

    #[test]
    fn a_cached_model_wins_over_the_prefetched_skeleton_on_a_warm_switch() {
        // Warm switches stay as they were: the model the project had when it was
        // left beats its (state-less) config skeleton.
        let mut st = test_state();
        let mut proj_skeleton = beta_config_skeleton();
        proj_skeleton.project_label = "proj skeleton".into();
        st.skeletons.lock().unwrap().insert("proj".into(), proj_skeleton);
        st.switch_project("beta");
        st.switch_project("proj");
        let model = st.sidebar_model.as_ref().unwrap();
        assert_eq!(model.project_label, "proj", "the cached model, not the skeleton");
        assert_eq!(model.workspaces[0].badge, shelbi_app::view::WorkspaceBadge::Idle);
    }

    #[test]
    fn a_cold_switch_never_waits_on_the_previous_projects_in_flight_reads() {
        // The cold-switch stall: the switch retired the old project's worker
        // bundle *inline*, and its `Drop` joins every refresher thread — so a read
        // still in flight (the launch project's cold activity-title hydration,
        // measured ~20s on a large remote board) froze the UI on the previous
        // project until it finished. Wedge every read of the launch project and
        // prove the switch returns at once with the target's skeleton painted.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let (g, started) = (gate.clone(), in_flight.clone());
        let factory: WorkersFactory = Arc::new(move |project: &str| {
            // A reader that blocks (up to 10s) until the gate opens, for the
            // launch project only; the target project's reads return at once.
            let wedge = {
                let (g, started, wedged) = (g.clone(), started.clone(), project == "proj");
                move || {
                    if wedged {
                        started.fetch_add(1, Ordering::SeqCst);
                        let (lock, cvar) = &*g;
                        let open = lock.lock().unwrap();
                        let _ = cvar.wait_timeout_while(open, Duration::from_secs(10), |o| !*o);
                    }
                }
            };
            let (w1, w2, w3) = (wedge.clone(), wedge.clone(), wedge);
            let (_ctx, change_rx) = std::sync::mpsc::channel();
            let (_ltx, layout_rx) = std::sync::mpsc::channel();
            ProjectWorkers {
                refresher: refresh::spawn(move |_g| {
                    w1();
                    ShellSnapshot { sidebar: None, board: None, activity: None, machines: None }
                }),
                activity_refresher: refresh::spawn(move |_g| {
                    w2();
                    ActivityData::tagged_for_test("x")
                }),
                closed_refresher: refresh::spawn(move |_g| {
                    w3();
                    None
                }),
                change_rx,
                layout_rx,
                _changes: None,
                _layout_sub: None,
            }
        });
        let mut st = test_state();
        st.make_workers = factory;
        st.start_project_workers();
        assert!(
            wait_until(Duration::from_secs(5), || in_flight.load(Ordering::SeqCst) == 3),
            "all three launch-project reads are in flight (and wedged)"
        );
        st.skeletons
            .lock()
            .unwrap()
            .insert("beta".into(), beta_config_skeleton());

        let t0 = Instant::now();
        st.switch_project("beta");
        let took = t0.elapsed();
        assert!(
            took < Duration::from_secs(2),
            "the switch must not wait on the old project's reads (took {took:?})"
        );
        let model = st.sidebar_model.as_ref().expect("the switch-frame model");
        assert_eq!(model.project_label, "Beta Board", "the target's skeleton, at once");

        // Release the retired readers so their threads wind down.
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
    }

    #[test]
    fn switching_projects_closes_the_previous_projects_review_interface() {
        // An open review interface renders over the sidebar column and main area
        // regardless of `main_view`, so a switch must drop it (and any in-flight
        // open) or the previous project's review stays painted over the new one.
        let mut st = test_state();
        st.main_view = MainView::Review("t-1".into());
        st.review = Some(ReviewInterface::new(
            "proj",
            st.connector.clone(),
            "t-1",
            "review-1",
            "/wt",
            "Vim",
            true,
            None,
        ));
        st.review_opening = Some("t-2".into());
        st.review_open_pending = Some("t-3".into());
        st.workspace_opening = Some("alpha".into());

        st.switch_project("beta");
        assert!(st.review.is_none(), "the old project's review interface is gone");
        assert!(st.workspace.is_none());
        assert!(st.review_opening.is_none() && st.review_open_pending.is_none());
        assert!(st.workspace_opening.is_none());
        assert!(!matches!(st.main_view, MainView::Review(_)));
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

    /// Fixed palette inputs for the smoke test: one other project and every
    /// edit target, read from nowhere.
    struct FixturePaletteInputs;

    impl PaletteInputs for FixturePaletteInputs {
        fn other_projects(&self, _project: &str) -> Vec<shelbi_app::command::ProjectItem> {
            vec![shelbi_app::command::ProjectItem {
                slug: "beta".into(),
                label: "Beta".into(),
            }]
        }
        fn edit_targets(&self, _project: &str) -> Vec<shelbi_app::command::EditItem> {
            use shelbi_app::command::EditItem;
            use shelbi_app::EditTarget;
            vec![
                EditItem {
                    target: EditTarget::Project,
                    title: "Edit Project Settings".into(),
                },
                EditItem {
                    target: EditTarget::Agent("developer".into()),
                    title: "Edit developer Settings".into(),
                },
                EditItem {
                    target: EditTarget::ZenMode,
                    title: "Edit Zen Mode".into(),
                },
                EditItem {
                    target: EditTarget::Workflows,
                    title: "Edit Workflows".into(),
                },
            ]
        }
        fn projects_column(&self, current: &str) -> Vec<shelbi_app::overlay::palette::ProjectEntry> {
            shelbi_app::command_model::projects_column_from(
                current,
                vec![("proj".into(), "proj".into()), ("beta".into(), "Beta".into())],
                &Default::default(),
            )
        }
    }

    /// A shell whose palette lists every kind of registry command: the nav
    /// views, a workspace, a review (with its approve / reject), another
    /// project, every edit target, and a second attached client. Effects are
    /// tapped, not performed.
    fn smoke_state() -> ShellState {
        let mut st = test_state();
        st.palette_inputs = Arc::new(FixturePaletteInputs);
        st.other_clients.store(1, Ordering::SeqCst);
        if let Some(model) = st.sidebar_model.as_mut() {
            model.reviews = vec![shelbi_app::view::ReviewRow {
                task_id: "T-9".into(),
                title: "Warm the cache".into(),
                branch: "jlong/T-9".into(),
                location: None,
                workspace: None,
                state: shelbi_app::view::ReviewState::Pending,
            }];
        }
        st.effect_tap = Some(Vec::new());
        st
    }

    fn type_key(st: &mut ShellState, c: char) {
        let mods = if c.is_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        st.handle_event(Event::Key(KeyEvent::new(KeyCode::Char(c), mods)));
    }

    fn screen_text(term: &Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    /// The TUI's palette smoke test: every command the registry offers is
    /// run through the real UI path (Ctrl+Space opens the palette, the title is
    /// typed, the arrows land on the entry, Enter runs it), the frame is drawn
    /// on ratatui's `TestBackend` with the entry on screen, and the command's
    /// effect is what reaches the executor.
    #[test]
    fn every_registry_command_runs_from_the_palette() {
        let mut st = smoke_state();
        let model = shelbi_app::command_model::command_model_from(
            "proj",
            st.sidebar_model.as_ref().unwrap(),
            true,
            FixturePaletteInputs.other_projects("proj"),
            FixturePaletteInputs.edit_targets("proj"),
        );
        let commands = CommandRegistry::new().commands(&model);
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
        let mut ran = Vec::new();
        for command in &commands {
            st.overlay = None;
            st.client.focus_main();
            st.handle_event(Event::Key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)));
            assert!(
                matches!(st.overlay, Some(ActiveOverlay::Palette(_))),
                "Ctrl+Space opens the palette for {}",
                command.id()
            );
            for c in command.title().chars() {
                type_key(&mut st, c);
            }
            // Walk the selection down to the entry (a fuzzy query can rank a
            // sibling first, as it would for a user).
            for _ in 0..commands.len() {
                let Some(ActiveOverlay::Palette(p)) = &st.overlay else {
                    panic!("the palette stays open while typing");
                };
                let results = p.results();
                let at = results
                    .iter()
                    .position(|(e, _)| e.id == command.id())
                    .unwrap_or_else(|| panic!("typing {:?} finds {}", command.title(), command.id()));
                if p.selected() == at {
                    break;
                }
                assert!(p.selected() < at, "{} is below the selection", command.id());
                st.handle_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)));
            }
            draw_frame(&mut term, &mut st).unwrap();
            let screen = screen_text(&term);
            let shown: String = command.title().chars().take(16).collect();
            assert!(screen.contains(&shown), "{} is on screen:\n{screen}", command.id());

            st.handle_event(Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
            assert!(st.overlay.is_none(), "Enter closes the palette for {}", command.id());
            let tap = st.effect_tap.as_ref().unwrap();
            assert_eq!(
                tap.last(),
                Some(&command.effect("proj")),
                "{} reached the executor",
                command.id()
            );
            ran.push(command.id().to_string());
        }
        // Every registry command, the ones hidden until typed included.
        assert_eq!(ran.len(), commands.len());
        for id in [
            "view:orch",
            "view:tasks",
            "view:activity",
            "view:machines",
            "action:toggle-zen",
            "workspace:alpha",
            "review:T-9",
            "action:approve-review:T-9",
            "action:reject-review:T-9",
            "action:error-log",
            "action:switch-project",
            "action:switch-project:beta",
            "action:add-project",
            "edit:project",
            "edit:agent:developer",
            "edit:zenmode",
            "edit:workflows",
            "action:detach",
            "action:detach-others",
            "action:quit-project",
            "action:quit-shelbi",
        ] {
            assert!(ran.iter().any(|r| r == id), "{id} was walked");
        }
    }

    #[test]
    fn a_bare_switch_project_opens_the_projects_column() {
        let mut st = smoke_state();
        st.effect_tap = None;
        st.dispatch_effect(Effect::SwitchProject {
            project: String::new(),
        });
        let Some(ActiveOverlay::Palette(p)) = &st.overlay else {
            panic!("the palette opens as the switcher");
        };
        assert!(p.projects_focused(), "on the Projects column");
        assert_eq!(st.client.project(), Some("proj"), "nothing switched yet");
    }

    #[test]
    fn an_edit_target_queues_the_editor_for_the_run_loop() {
        let mut st = smoke_state();
        st.effect_tap = None;
        st.dispatch_effect(Effect::OpenEditor {
            target: shelbi_app::EditTarget::ZenMode,
        });
        let path = st.pending_editor.clone().expect("the editor is queued");
        assert!(path.ends_with("zenmode.md"), "{}", path.display());
    }

    #[test]
    fn shell_chrome_renders_readable_in_light() {
        use shelbi_app::theme::Appearance;
        crate::theme::with_appearance(Appearance::Light, || {
            let area = Rect::new(0, 0, 60, 6);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            render_session_lost_banner(&mut buf, Rect::new(0, 0, 60, 1), "login session lost");
            render_notice(&mut buf, Rect::new(0, 1, 60, 1), "keyboard protocol notice");
            render_status(&mut buf, Rect::new(0, 2, 60, 2), "starting orchestrator…");
            render_divider(&mut buf, Rect::new(0, 4, 20, 1), false);
            render_divider(&mut buf, Rect::new(0, 5, 20, 1), true);
            crate::theme::assert_light_readable("shell-chrome", &buf);
        });
    }
}
