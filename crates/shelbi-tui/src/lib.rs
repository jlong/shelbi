//! shelbi's two top-level entry points:
//!
//! - `run_main(project)` — set up the project's tmux session with the
//!   dashboard layout (sidebar + orchestrator) and `exec tmux attach`.
//!   This is what `shelbi` (no subcommand) invokes.
//! - `run_sidebar(project)` — the minimal ratatui process that lives in
//!   the dashboard's left pane: agent list, status footer, Ctrl+Space
//!   palette.
//!   Selecting an agent switches the tmux window. This is what
//!   `shelbi __sidebar PROJECT` invokes.

use std::io;

use anyhow::{Context, Result};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

mod activity;
mod app;
mod handlers;
mod kanban;
mod keymap;
mod markdown;
mod poller;
mod review;
mod sidebar;
mod zen_probe;

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Serializes any test that mutates the process-global `SHELBI_HOME`
    /// env var. Tests across modules share one binary (and thus one env),
    /// so they must all lock the *same* mutex or they race each other.
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Provision a real git repo + project YAML at `<home>/projects/<name>.yaml`
    /// pointing the hub machine at the repo. The kanban TUI's
    /// `move_card` now runs the depends_on-aware branch cut via
    /// `shelbi_orchestrator::lifecycle` when a task lands in
    /// `in_progress`; that hook needs a loadable project YAML and a
    /// real git repo at the hub workdir. Tests reach for this helper to
    /// produce both.
    ///
    /// Caller must hold `ENV_LOCK` and have already pointed
    /// `SHELBI_HOME` at `home`. Returns the repo path so the test can
    /// drive further git operations against it.
    pub fn provision_hub_repo_for_project(home: &Path, project_name: &str) -> PathBuf {
        use shelbi_core::{
            AgentRunnerSpec, GitConfig, HeartbeatConfig, Machine, MachineKind,
            OrchestratorSpec, Project, ZenConfig,
        };
        use std::collections::BTreeMap;
        use std::process::Command;

        let repo = home.join(format!("{project_name}-repo"));
        std::fs::create_dir_all(&repo).unwrap();

        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed in {}", repo.display());
        };
        run(&["init", "-q", "-b", "main", "."]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "init"]);

        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                dialog_signatures: vec![],
            },
        );
        let project = Project {
            name: project_name.into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.clone(),
                host: None,
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: Vec::new(),
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
            git: GitConfig::default(),
            review: Default::default(),
        };
        shelbi_state::save_project(&project).unwrap();
        repo
    }
}

pub use activity::ActivityApp;
pub use app::{App, Row, View, WorkspaceBadge, WorkspaceOverview};
pub use kanban::KanbanApp;
pub use poller::WorkspacePoller;
pub use review::ReviewApp;
pub use sidebar::decoration_to_color;

/// Set up the project's tmux session and attach to it. If we're already
/// inside a tmux client, use `switch-client` instead of `attach` (tmux
/// refuses to nest, modern tmux supports switching).
pub fn run_main(project_name: &str) -> Result<()> {
    // Bump the recently-used timestamp before bootstrapping the session
    // so the picker's recency sort reflects this launch even if the
    // tmux exec below replaces the process before normal shutdown.
    // Best-effort — a missing/unwritable ~/.shelbi/shelbi.yaml should
    // not block launching.
    let _ = shelbi_state::touch_project_launched(project_name);

    shelbi_orchestrator::ensure_dashboard(project_name)
        .with_context(|| format!("setting up dashboard for `{project_name}`"))?;

    let session = format!("shelbi-{project_name}");
    let inside_tmux = std::env::var("TMUX").is_ok();

    let args: &[&str] = if inside_tmux {
        &["switch-client", "-t"]
    } else {
        &["attach", "-t"]
    };

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new("tmux")
            .args(args)
            .arg(&session)
            .exec();
        Err(err.into())
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new("tmux")
            .args(args)
            .arg(&session)
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux exited with {status}");
        }
        Ok(())
    }
}

/// Run the minimal ratatui sidebar in the current pane.
pub fn run_sidebar(project_name: &str) -> Result<()> {
    // Load merged keymaps once — embedded builtins, then
    // `~/.shelbi/keys.yaml::defaults`, then `projects.<project_name>`.
    // Diagnostics route through `tracing` so they land in
    // `~/.shelbi/logs/tui.log` instead of fighting ratatui for the pane
    // TTY (eprintln! into the alt-screen pane interleaves with the
    // sidebar redraw and corrupts the nav labels). Resolve them before
    // `setup_terminal` so the count is ready for the status line.
    let (keymaps, diags) = shelbi_state::keymap::load_keymaps(Some(project_name));
    let startup_warnings = log_keymap_diagnostics(&diags);

    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = App::new_sidebar(project_name);
    app.refresh().ok();
    if startup_warnings > 0 {
        app.status_line = startup_warnings_status_line(startup_warnings);
    }

    // First-run probe: on fresh installs (no ~/.shelbi/config.yaml),
    // verify Alt+Z is delivered and let the user pick a fallback if not.
    // Best-effort: an error here defaults to Alt+Z so the sidebar still
    // launches with a working binding on cooperative terminals.
    let probe_chord = zen_probe::ensure_zen_keymap(&mut term)
        .unwrap_or(shelbi_state::ZenToggleChord::AltZ);
    // Prefer the keys.yaml-resolved chord (so a migrated `zen_toggle`
    // shows the right glyph even though `config.yaml` is now at default)
    // and fall back to the probe's answer for chords the four-value
    // [`ZenToggleChord`] enum can't represent.
    app.zen_toggle_chord = keymaps.zen_toggle_chord(probe_chord);
    app.keymaps = keymaps;

    // Background poll loop: per-workspace `tmux display-message` every
    // `workspace_poll_interval_secs`, parses the `shelbi:<state>` marker,
    // persists transitions to `~/.shelbi/workspaces/<name>/status.yaml`
    // and `~/.shelbi/events.log`. The handle's Drop joins the thread,
    // so it shuts down when this function returns regardless of which
    // exit path we took.
    let _poller = WorkspacePoller::start(project_name);

    let result = handlers::sidebar::sidebar_loop(&mut term, &mut app);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

/// Run the Kanban tasks view in the current pane. Meant to be hosted in
/// the project's hidden stash session and swapped into the dashboard via
/// the palette. Parent shell wraps invocation in `while true; do …; done`
/// so an accidental crash respawns instead of leaving an empty pane.
pub fn run_tasks(project_name: &str) -> Result<()> {
    // Load `keys.yaml` before the alt-screen swap. Diagnostics route
    // through `tracing` (→ `~/.shelbi/logs/tui.log`) so they can't
    // interleave with ratatui's redraw on the shared pane TTY. Bad
    // config never blocks launch — affected actions fall back to
    // built-in defaults. The sidebar pane surfaces a discoverable
    // count in its status line.
    let (keymaps, diags) = shelbi_state::keymap::load_keymaps(Some(project_name));
    log_keymap_diagnostics(&diags);

    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = KanbanApp::new(project_name);
    // Hand the footer renderer its own copy of the resolved keymaps; the
    // handler keeps the `&keymaps` local below to dodge a borrow conflict
    // with `&mut app`.
    app.keymaps = keymaps.clone();
    app.refresh();

    let result = handlers::kanban::tasks_loop(&mut term, &mut app, &keymaps);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

/// Run the review-queue ratatui view in the current pane. Hosted in the
/// hidden stash session and swapped in by the palette / sidebar — same
/// lifecycle as `run_tasks`.
pub fn run_review(project_name: &str) -> Result<()> {
    // Load `keys.yaml` before the alt-screen swap. Diagnostics route
    // through `tracing` (→ `~/.shelbi/logs/tui.log`) so they can't
    // interleave with ratatui's redraw on the shared pane TTY. Bad
    // config never blocks launch — affected actions fall back to
    // built-in defaults. The sidebar pane surfaces a discoverable
    // count in its status line.
    let (keymaps, diags) = shelbi_state::keymap::load_keymaps(Some(project_name));
    log_keymap_diagnostics(&diags);

    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = ReviewApp::new(project_name);
    // Hand the footer renderer its own copy of the resolved keymaps; the
    // handler keeps the `&keymaps` local below to dodge a borrow conflict
    // with `&mut app`.
    app.keymaps = keymaps.clone();
    app.refresh();

    let result = handlers::review::review_loop(&mut term, &mut app, &keymaps);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

/// Run the activity-feed ratatui view in the current pane. Hosted in
/// the hidden stash session and swapped in by the palette / sidebar —
/// same lifecycle as `run_tasks` and `run_review`.
pub fn run_activity(project_name: &str) -> Result<()> {
    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = ActivityApp::new(project_name);
    app.refresh();

    let result = handlers::activity::activity_loop(&mut term, &mut app);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

/// Enter raw mode + alt screen + mouse capture. Tmux only forwards mouse
/// events to the pane when its `mouse` option is on — `ensure_dashboard`
/// sets it on shelbi sessions, so callers don't need to plumb anything.
/// Views that don't care about mouse just ignore the events.
fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    term: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

/// Route every keys.yaml load diagnostic through `tracing` so the TUI's
/// `init_tracing` writer drops them into `~/.shelbi/logs/tui.log`
/// instead of the shared pane TTY. Direct `eprintln!` after the
/// alt-screen swap collides with ratatui's redraw cycle and corrupts
/// whichever cells happen to be re-painting at the same moment — the
/// observed failure mode is a sidebar with the warning text spliced
/// through the nav labels. Returns the diagnostic count so the caller
/// can surface a discoverable "⚠ N startup warnings" hint.
fn log_keymap_diagnostics(diags: &[shelbi_state::keymap::KeymapDiagnostic]) -> usize {
    use shelbi_state::keymap::KeymapDiagnostic;
    for d in diags {
        match d {
            KeymapDiagnostic::Error { message, location, .. } => match location {
                Some(loc) => tracing::error!("keys.yaml error: {message} (at {loc})"),
                None => tracing::error!("keys.yaml error: {message}"),
            },
            KeymapDiagnostic::Warning { message, location, .. } => match location {
                Some(loc) => tracing::warn!("keys.yaml warning: {message} (at {loc})"),
                None => tracing::warn!("keys.yaml warning: {message}"),
            },
        }
    }
    diags.len()
}

/// Build the sidebar status-line text that surfaces a startup-warning
/// count and points the user at the log file where the full diagnostic
/// text lives. Kept tiny so the line still fits on a narrow sidebar.
fn startup_warnings_status_line(count: usize) -> String {
    let suffix = if count == 1 { "" } else { "s" };
    format!("⚠ {count} startup warning{suffix} — see ~/.shelbi/logs/tui.log")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use shelbi_state::keymap::{ErrorKind, KeymapDiagnostic, WarningKind};

    fn warning(message: &str, location: Option<&str>) -> KeymapDiagnostic {
        KeymapDiagnostic::Warning {
            kind: WarningKind::LegacyZenToggleField,
            message: message.into(),
            location: location.map(str::to_string),
        }
    }

    fn error(message: &str, location: Option<&str>) -> KeymapDiagnostic {
        KeymapDiagnostic::Error {
            kind: ErrorKind::UnknownAction,
            message: message.into(),
            location: location.map(str::to_string),
        }
    }

    /// The diagnostic-routing helper returns the diagnostic count. The
    /// caller uses this to drive the sidebar's status-line surface; if
    /// the count drifts the hint silently disappears.
    #[test]
    fn log_keymap_diagnostics_returns_count() {
        let diags = vec![
            warning("legacy zen_toggle field", Some("config.yaml")),
            error("unknown action `nope`", None),
        ];
        assert_eq!(log_keymap_diagnostics(&diags), 2);
        assert_eq!(log_keymap_diagnostics(&[]), 0);
    }

    /// Plural / singular suffix on the status-line copy. One warning
    /// reads "1 startup warning", two read "2 startup warnings"; both
    /// point at the log file so the user knows where to look.
    #[test]
    fn startup_warnings_status_line_uses_correct_plural() {
        let one = startup_warnings_status_line(1);
        assert!(one.contains("1 startup warning "), "{one}");
        assert!(one.contains("~/.shelbi/logs/tui.log"), "{one}");
        let many = startup_warnings_status_line(3);
        assert!(many.contains("3 startup warnings "), "{many}");
    }

    /// Regression for the startup-warnings-interleave bug. The sidebar
    /// must render its three nav labels (`💬 Chat`, `📋 Tasks`,
    /// `⚡ Activity`) uninterrupted even when a keys.yaml load produced
    /// warnings. The pre-fix code `eprintln!`'d after the alt-screen
    /// swap and the diagnostic text landed mid-label; the new path
    /// routes diagnostics to `tracing` and surfaces a count in the
    /// status line, so the nav labels stay clean.
    #[test]
    fn sidebar_nav_labels_render_uninterrupted_with_startup_warnings() {
        let diags = vec![
            warning(
                "config.yaml::keymap.zen_toggle has no keys.yaml::default for zen_toggle",
                Some("config.yaml"),
            ),
        ];
        let count = log_keymap_diagnostics(&diags);
        let mut app = App::new_sidebar("demo");
        if count > 0 {
            app.status_line = startup_warnings_status_line(count);
        }

        let backend = TestBackend::new(60, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| sidebar::render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let dumped: Vec<String> = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect();
        let joined = dumped.join("\n");

        // Each nav label appears once, contiguously, in the rendered
        // pane. TestBackend reserves a filler cell after each wide
        // emoji glyph, so the buffer dump shows two spaces between the
        // icon and the label text — that's the layout the user sees on
        // a real terminal too, just collapsed to one cell.
        for (emoji, text) in [("💬", "Chat"), ("📋", "Tasks"), ("⚡", "Activity")] {
            let label = format!("{emoji}  {text}");
            assert!(
                joined.matches(&label).count() == 1,
                "expected `{label}` to render exactly once and contiguously, but got:\n{joined}",
            );
        }

        // And the status line surfaces a discoverable count.
        assert!(
            joined.contains("⚠ 1 startup warning"),
            "expected startup-warning hint in:\n{joined}",
        );
        assert!(
            joined.contains("~/.shelbi/logs/tui.log"),
            "expected log-file pointer in:\n{joined}",
        );

        // No raw diagnostic text leaks onto the pane — only the count.
        assert!(
            !joined.contains("zen_toggle"),
            "raw diagnostic text leaked onto the pane:\n{joined}",
        );
    }

    /// Regression for the agents-workspaces variant of the
    /// startup-warnings-interleave bug. A project YAML still using the
    /// legacy `workers:` top-level key fires
    /// `shelbi_state::warn_legacy_workers_key`; before the fix this was
    /// an `eprintln!` that landed on the shared pane TTY mid-refresh
    /// (the sidebar `App::refresh` path calls `load_project` on every
    /// poll), splicing fragments of `shelbi: project \`<name>\` uses
    /// the legacy \`workers:\`…` through ratatui's nav labels. The fix
    /// routes the warning through `tracing::warn!` so the TUI's
    /// file-backed writer captures it. This test asserts the contract
    /// the render path now relies on: a refresh against a legacy
    /// `workers:` YAML leaves the nav labels intact and does not
    /// surface the deprecation copy anywhere in the rendered buffer.
    #[test]
    fn sidebar_renders_cleanly_when_project_yaml_uses_legacy_workers_key() {
        let _g = test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-sidebar-legacy-workers-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        // Hand-author the project YAML with the legacy `workers:` key.
        // `save_project` would round-trip through the canonical
        // `workspaces:` form; we deliberately exercise the loader path
        // that fires the deprecation warning.
        let projects_dir = home.join("projects");
        std::fs::create_dir_all(&projects_dir).unwrap();
        let yaml = "\
name: demo
repo: /tmp/demo-legacy-workers
default_branch: main
machines:
  - name: hub
    kind: local
    work_dir: /tmp/demo-legacy-workers
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
workers:
  - name: alpha
    machine: hub
    runner: claude
workspace_poll_interval_secs: 5
workspace_permissions_mode: auto
";
        std::fs::write(projects_dir.join("demo.yaml"), yaml).unwrap();

        let mut app = App::new_sidebar("demo");
        let _ = app.refresh();

        let backend = TestBackend::new(60, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| sidebar::render_full(f, &mut app, f.area()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let joined: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");

        for (emoji, text) in [("💬", "Chat"), ("📋", "Tasks"), ("⚡", "Activity")] {
            let label = format!("{emoji}  {text}");
            assert!(
                joined.matches(&label).count() == 1,
                "expected `{label}` to render exactly once and contiguously, but got:\n{joined}",
            );
        }

        // The deprecation copy must not surface in the render buffer
        // through any side channel (status line, error pane, etc.) —
        // it belongs in the log file, not on the sidebar TTY. Each
        // needle is a distinctive fragment of the warning message that
        // would only land here via a regression.
        for needle in ["workers:", "legacy", "future release"] {
            assert!(
                !joined.contains(needle),
                "deprecation warning text leaked onto the sidebar pane:\n  \
                 needle = {needle:?}\n  buffer:\n{joined}",
            );
        }

        std::env::remove_var("SHELBI_HOME");
    }
}
