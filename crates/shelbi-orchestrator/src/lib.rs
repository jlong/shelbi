//! Project tmux session bootstrap.
//!
//! Each shelbi project owns one tmux session named `shelbi-<project>`. Its
//! first window is `dashboard`, a two-pane layout:
//!
//! - left pane (small): the `shelbi __sidebar <project>` ratatui process —
//!   nav, agent list, Ctrl+Space palette.
//! - right pane: the configured orchestrator agent CLI (e.g. `claude`),
//!   running natively in the pane. The user types into it directly.
//!
//! Worker agents are additional windows in the same session (local) or
//! their own `shelbi-w-<id>` sessions on a remote machine (so they survive
//! SSH disconnect). The `shelbi orchestrate` CLI and the TUI launcher both
//! call into `ensure_dashboard()` so the bootstrap is idempotent and
//! consistent.

use shelbi_core::{Error, Host, MachineKind, Result, TmuxAddr};

pub mod review;
pub mod worker;

pub const DEFAULT_SYSTEM_PROMPT: &str = include_str!("default_orchestrator.md.template");

// Sidebar pane width is clamped to this char range. Below the min the
// footer hint (`  ^P palette  Enter focus`, 24 chars) starts to
// truncate; above the max the orchestrator pane loses room without the
// sidebar gaining anything useful. Within the range the sidebar tracks
// `SIDEBAR_TARGET_PCT` of the window width — chosen so the
// orchestrator gets noticeably more room on both narrow and wide
// terminals than the previous fixed 30% split.
const SIDEBAR_MIN_COLS: u32 = 24;
const SIDEBAR_MAX_COLS: u32 = 40;
const SIDEBAR_TARGET_PCT: u32 = 25;

/// Outcome of `ensure_dashboard`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapStatus {
    AlreadyRunning,
    Started,
}

/// Per-pane outcome for `reload`. Each pane is independent: the report
/// records what was found and whether the respawn succeeded.
#[derive(Debug, Default, Clone)]
pub struct ReloadReport {
    pub sidebar: PaneReloadStatus,
    pub tasks: PaneReloadStatus,
    pub review: PaneReloadStatus,
    pub machines: PaneReloadStatus,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum PaneReloadStatus {
    #[default]
    NotAttempted,
    Respawned {
        target: String,
    },
    Missing,
    Failed {
        target: String,
        reason: String,
    },
}

/// Resolve the active orchestrator system prompt for a project: per-project
/// override (`ORCHESTRATOR.md`) if present, else the bundled default.
/// The string `{{assistant_name}}` is substituted with the user's chosen
/// assistant name from `~/.shelbi/shelbi.yaml` (falling back to the
/// default `Orchestrator` when the wizard hasn't run yet).
pub fn system_prompt(project: &str) -> Result<String> {
    let path = shelbi_state::project_dir(project)?.join("ORCHESTRATOR.md");
    let raw = if path.exists() {
        std::fs::read_to_string(&path).map_err(Error::Io)?
    } else {
        DEFAULT_SYSTEM_PROMPT.to_string()
    };
    let cfg = shelbi_state::load_shelbi_config()?;
    Ok(raw.replace("{{assistant_name}}", cfg.assistant_name()))
}

/// The dashboard window's tmux address (orchestrator's session).
pub fn dashboard_addr(project_name: &str) -> TmuxAddr {
    TmuxAddr {
        session: format!("shelbi-{project_name}"),
        window: "dashboard".into(),
    }
}

/// Swap the named view's pane into the dashboard's right slot. `view` is
/// one of `orch`, `tasks`, `review`, `machines`. Reads the stored pane id
/// from the session's tmux environment.
pub fn show_view(project_name: &str, view: &str) -> Result<()> {
    let session = format!("shelbi-{project_name}");
    let key = format!("SHELBI_PANE_{view}");

    // `show-environment -t session KEY` prints `KEY=value` (or `-KEY` if
    // unset). Parse it.
    let out = std::process::Command::new("tmux")
        .args(["show-environment", "-t", &session, &key])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "view `{view}` has no stored pane id ({}); is shelbi set up for this session?",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim();
    let Some((_k, pane_id)) = line.split_once('=') else {
        return Err(Error::Other(format!("unexpected tmux env output: {line}")));
    };
    if pane_id.is_empty() {
        return Err(Error::Other(format!("empty pane id for `{view}`")));
    }

    // Swap the target pane into the dashboard's right slot.
    let dashboard = format!("{session}:dashboard.{{right}}");
    let _ = std::process::Command::new("tmux")
        .args(["swap-pane", "-s", pane_id, "-t", &dashboard])
        .status()
        .map_err(Error::Io)?;
    // Make sure focus lands on the now-visible view.
    let _ = std::process::Command::new("tmux")
        .args(["select-window", "-t", &format!("{session}:dashboard")])
        .status();
    let _ = std::process::Command::new("tmux")
        .args(["select-pane", "-t", &dashboard])
        .status();
    Ok(())
}

/// Focus the dashboard window on the declared worker's pane.
///
/// Local workers live in a window named after the worker inside the
/// project session (placed there by `shelbi task start`). Remote workers
/// live in their own tmux session on the remote machine — we surface them
/// by maintaining a *proxy window* in the project session, named after
/// the worker, whose command is `ssh -t <host> tmux attach -t
/// shelbi-w-<worker>`. The proxy is created lazily on first selection and
/// re-used on subsequent selections; closing it (e.g. detaching from the
/// remote tmux) lets the next selection spawn a fresh one.
///
/// Single source of truth for the sidebar's Enter-on-worker behavior and
/// the Ctrl+P palette's worker entries — both call here so they can't
/// drift.
pub fn focus_worker(project_name: &str, worker_name: &str) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let worker = project.worker(worker_name).ok_or_else(|| {
        Error::Other(format!(
            "worker `{worker_name}` not declared in project YAML"
        ))
    })?;
    let machine = project.machine(&worker.machine).ok_or_else(|| {
        Error::Other(format!(
            "worker `{worker_name}` references unknown machine `{}`",
            worker.machine
        ))
    })?;

    let project_session = format!("shelbi-{project_name}");
    let target = format!("{project_session}:{}", worker.name);

    // Window already in the project session — local worker window OR a
    // remote proxy window we created earlier. Just switch to it.
    if run_local_tmux(["select-window", "-t", &target]) {
        return Ok(());
    }

    match machine.host() {
        Host::Local => Err(Error::Other(format!(
            "worker has no live pane — assign a task with \
             `shelbi task start <task> --worker {worker_name}`"
        ))),
        Host::Ssh { host } => {
            let remote_session = format!("shelbi-w-{}", worker.name);
            let cmd = format!(
                "ssh -t {host} tmux attach -t {remote_session}",
                host = shelbi_agent::shell_escape(&host),
                remote_session = shelbi_agent::shell_escape(&remote_session),
            );
            let ok = run_local_tmux([
                "new-window",
                "-t",
                &format!("{project_session}:"),
                "-n",
                &worker.name,
                "sh",
                "-c",
                &cmd,
            ]);
            if !ok {
                return Err(Error::Other(format!(
                    "couldn't open proxy window for remote worker `{worker_name}` on `{host}`"
                )));
            }
            let _ = run_local_tmux(["select-window", "-t", &target]);
            Ok(())
        }
    }
}

fn run_local_tmux<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    std::process::Command::new("tmux")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Idempotently set up the project's tmux session with a `dashboard`
/// window split into sidebar (left) + orchestrator (right). Safe to call
/// repeatedly.
pub fn ensure_dashboard(project_name: &str) -> Result<BootstrapStatus> {
    let project = shelbi_state::load_project(project_name)?;

    let hub = project
        .machines
        .iter()
        .find(|m| matches!(m.kind, MachineKind::Local))
        .ok_or_else(|| {
            Error::Other(format!("project `{project_name}` has no local hub machine"))
        })?;
    let host = hub.host();

    let runner_spec = project
        .runner(&project.orchestrator.runner)
        .ok_or_else(|| {
            Error::Other(format!(
                "orchestrator runner `{}` not declared in project `{project_name}`",
                project.orchestrator.runner
            ))
        })?
        .clone();

    let addr = dashboard_addr(project_name);
    let session = &addr.session;
    let dashboard = format!("{session}:dashboard");

    // Install the session-closed cleanup hook before doing anything else.
    // Idempotent and project-agnostic — set every ensure_dashboard call so
    // it survives shelbi upgrades and tmux-server restarts.
    install_stash_cleanup_hook(&host)?;

    // Materialize the orchestrator's workdir + CLAUDE.md upfront — needed
    // whether we create the session from scratch or just the right pane.
    let workdir = shelbi_state::project_dir(project_name)?;
    shelbi_state::ensure_dir(&workdir)?;
    let prompt = system_prompt(project_name)?;
    std::fs::write(workdir.join("CLAUDE.md"), &prompt).map_err(Error::Io)?;

    // Drop the sidebar-clamp script. The bootstrapped hooks invoke it
    // via `sh <path>` — keeping the body in a file dodges all of the
    // tmux double-quote / $VAR / #{...} escape gymnastics that fighting
    // the same logic inline would require.
    let clamp_script_path = workdir.join("sidebar-clamp.sh");
    std::fs::write(&clamp_script_path, sidebar_clamp_script(session))
        .map_err(Error::Io)?;

    let shelbi_bin = current_exe_string()?;
    let sidebar_cmd_str = sidebar_cmd(&shelbi_bin, project_name);
    let launch = shelbi_agent::launch_command(&runner_spec);
    let orch_cmd = format!(
        "cd {wd} && SHELBI_PROJECT={proj} SHELBI_TMUX_SESSION={sess} exec {launch}",
        wd = shelbi_agent::shell_escape(&workdir.to_string_lossy()),
        proj = shelbi_agent::shell_escape(project_name),
        sess = shelbi_agent::shell_escape(session),
    );

    // 1. Ensure the project session exists with a `dashboard` window whose
    //    initial pane runs the sidebar directly (no send-keys race).
    if !shelbi_tmux::has_session(&host, session)? {
        shelbi_ssh::run_capture(
            &host,
            [
                "tmux",
                "new-session",
                "-d",
                "-s",
                session,
                "-n",
                "dashboard",
                "sh",
                "-c",
                &sidebar_cmd_str,
            ],
        )?;
    } else {
        let windows = shelbi_ssh::run_capture(
            &host,
            ["tmux", "list-windows", "-t", session, "-F", "#W"],
        )?;
        if !windows.lines().any(|w| w.trim() == "dashboard") {
            shelbi_ssh::run_capture(
                &host,
                [
                    "tmux",
                    "new-window",
                    "-d",
                    "-t",
                    &format!("{session}:"),
                    "-n",
                    "dashboard",
                    "sh",
                    "-c",
                    &sidebar_cmd_str,
                ],
            )?;
        }
    }

    // Enable mouse on the project session so sidebar clicks and scroll
    // wheel reach the ratatui pane. Scoped to this session — won't disturb
    // mouse behavior in the user's other tmux sessions. Idempotent; safe
    // to call every bootstrap.
    let _ = shelbi_ssh::run_capture(
        &host,
        ["tmux", "set-option", "-t", session, "mouse", "on"],
    );

    // 2. If the dashboard already has 2+ panes, layout is set up.
    let panes = shelbi_ssh::run_capture(
        &host,
        ["tmux", "list-panes", "-t", &dashboard, "-F", "#P"],
    )?;
    let pane_count = panes.lines().filter(|l| !l.trim().is_empty()).count();
    if pane_count >= 2 {
        return Ok(BootstrapStatus::AlreadyRunning);
    }

    // Install the Ctrl+P tmux binding for the palette popup. The binding
    // itself is server-scoped (tmux has no session-local key bindings),
    // but the action is gated on the session name: outside a `shelbi-*`
    // session the keystroke is passed straight through with `send-keys`
    // so the user's other tmux sessions see Ctrl-P with no behavior
    // change. Gone if the tmux server restarts.
    let popup_cmd = format!("{} popup", shelbi_agent::shell_escape(&shelbi_bin));
    let _ = shelbi_ssh::run(
        &host,
        [
            "tmux",
            "bind-key",
            "-n",
            "C-p",
            "if-shell",
            "-F",
            "#{m:shelbi-*,#{session_name}}",
            &format!("run-shell \"{popup_cmd}\""),
            "send-keys C-p",
        ],
    );

    // 3. Split the dashboard window: orchestrator on the right.
    //    Initial split is 50/50 — the sidebar-clamp hooks installed
    //    below set the final sizing as soon as a client attaches (or
    //    immediately, if we're being run from inside one).
    //    `-P -F #{pane_id}` echoes the new pane's stable ID (e.g. `%42`)
    //    which we'll stash in a session env var so the sidebar / palette
    //    can swap it back in by ID later.
    let orch_pane_id = shelbi_ssh::run_capture(
        &host,
        [
            "tmux",
            "split-window",
            "-h",
            "-t",
            &dashboard,
            "-P",
            "-F",
            "#{pane_id}",
            "sh",
            "-c",
            &orch_cmd,
        ],
    )?;
    let orch_pane_id = orch_pane_id.trim().to_string();
    set_session_env(&host, session, "SHELBI_PANE_orch", &orch_pane_id)?;

    // Focus the orchestrator pane so the user can type immediately.
    shelbi_ssh::run_capture(
        &host,
        ["tmux", "select-pane", "-t", &format!("{dashboard}.{{right}}")],
    )?;

    // Bound the sidebar to a sane char-width range so it neither
    // bloats on wide terminals nor cramps the orchestrator on narrow
    // ones. The hooks re-clamp on every client resize (including the
    // first attach); the one-shot below covers the in-tmux
    // `switch-client` path, where no attach event fires.
    install_sidebar_clamp_hooks(&host, session, &clamp_script_path)?;
    let _ = clamp_sidebar(&host, &clamp_script_path);

    // 4. Materialize the hidden `__views` window with tasks/review/machines
    //    panes. Each runs a tiny watch loop or one-shot script. Sidebar
    //    swaps them into the dashboard's right pane via `tmux swap-pane`.
    create_hidden_views(&host, session, project_name, &shelbi_bin)?;

    Ok(BootstrapStatus::Started)
}

fn create_hidden_views(
    host: &shelbi_core::Host,
    session: &str,
    project_name: &str,
    shelbi_bin: &str,
) -> Result<()> {
    // Stash lives in a separate session — `_shelbi-<project>` — so the
    // user never sees a `__views` window in their visible session's
    // window list. Pane IDs are global in tmux, so swap-pane across
    // sessions works just like within one.
    let stash = format!("_{session}");

    // Already exists? Skip (idempotent).
    if shelbi_tmux::has_session(host, &stash)? {
        return Ok(());
    }

    let tasks_cmd_str = tasks_cmd(shelbi_bin, project_name);
    let review_cmd_str = review_cmd(shelbi_bin, project_name);
    let machines_cmd_str = machines_cmd(shelbi_bin, project_name);

    // Create the stash session detached, with tasks pane.
    let tasks_id = shelbi_ssh::run_capture(
        host,
        [
            "tmux", "new-session", "-d", "-s", &stash, "-n", "views",
            "-P", "-F", "#{pane_id}",
            "sh", "-c", &tasks_cmd_str,
        ],
    )?;
    let tasks_id = tasks_id.trim().to_string();

    let stash_win = format!("{stash}:views");

    let review_id = shelbi_ssh::run_capture(
        host,
        [
            "tmux", "split-window", "-v", "-t", &stash_win,
            "-P", "-F", "#{pane_id}",
            "sh", "-c", &review_cmd_str,
        ],
    )?;
    let review_id = review_id.trim().to_string();

    let machines_id = shelbi_ssh::run_capture(
        host,
        [
            "tmux", "split-window", "-v", "-t", &stash_win,
            "-P", "-F", "#{pane_id}",
            "sh", "-c", &machines_cmd_str,
        ],
    )?;
    let machines_id = machines_id.trim().to_string();

    // Env vars live on the *visible* session — that's where show_view reads
    // them from. swap-pane finds the target pane by global pane id anyway.
    set_session_env(host, session, "SHELBI_PANE_tasks", &tasks_id)?;
    set_session_env(host, session, "SHELBI_PANE_review", &review_id)?;
    set_session_env(host, session, "SHELBI_PANE_machines", &machines_id)?;
    Ok(())
}

fn set_session_env(
    host: &shelbi_core::Host,
    session: &str,
    key: &str,
    value: &str,
) -> Result<()> {
    shelbi_ssh::run_capture(host, ["tmux", "set-environment", "-t", session, key, value])?;
    Ok(())
}

/// Shell snippet that queries the dashboard window's current width via
/// tmux, computes `SIDEBAR_TARGET_PCT%` clamped to `[MIN, MAX]`, and
/// resizes the left (sidebar) pane to that. Written to disk so the
/// hook can invoke it by path without inlining shell into a tmux
/// command-list string.
fn sidebar_clamp_script(session: &str) -> String {
    format!(
        "#!/bin/sh\n\
         # Auto-generated by shelbi; rewritten on every `ensure_dashboard`.\n\
         w=$(tmux display-message -p -t '{sess}:dashboard' '#{{window_width}}' 2>/dev/null)\n\
         [ -z \"$w\" ] && exit 0\n\
         c=$((w * {pct} / 100))\n\
         [ \"$c\" -lt {min} ] && c={min}\n\
         [ \"$c\" -gt {max} ] && c={max}\n\
         tmux resize-pane -t '{sess}:dashboard.{{left}}' -x \"$c\" 2>/dev/null || true\n",
        sess = session,
        pct = SIDEBAR_TARGET_PCT,
        min = SIDEBAR_MIN_COLS,
        max = SIDEBAR_MAX_COLS,
    )
}

/// Install `client-attached` and `client-resized` hooks on the session
/// so the sidebar pane is re-clamped to `[MIN, MAX]` cols every time
/// the client's terminal size changes. Without this the pane would
/// scale proportionally with the window, which is exactly what we're
/// trying to avoid.
fn install_sidebar_clamp_hooks(
    host: &shelbi_core::Host,
    session: &str,
    script_path: &std::path::Path,
) -> Result<()> {
    let path_esc = shelbi_agent::shell_escape(&script_path.to_string_lossy());
    let hook_cmd = format!("run-shell -b 'sh {path_esc}'");
    for event in ["client-attached", "client-resized"] {
        let _ = shelbi_ssh::run(
            host,
            ["tmux", "set-hook", "-t", session, event, &hook_cmd],
        );
    }
    Ok(())
}

/// Run the clamp once now, for the in-tmux `switch-client` path where
/// no `client-attached` fires. Best-effort; failures are silent because
/// any real client interaction will re-trigger the hook.
fn clamp_sidebar(host: &shelbi_core::Host, script_path: &std::path::Path) -> std::io::Result<()> {
    let path = script_path.to_string_lossy();
    shelbi_ssh::run(host, ["sh", path.as_ref()]).map(|_| ())
}

/// Install a global `session-closed` hook on the tmux server so that when
/// the user kills a `shelbi-<project>` session its `_shelbi-<project>`
/// stash gets cleaned up too. The pattern `shelbi-*` ignores the stash
/// itself (`_shelbi-*`), so the hook can't recurse. Uses hook array index
/// 42 to avoid clobbering any unrelated `session-closed` hooks the user
/// may have set.
fn install_stash_cleanup_hook(host: &shelbi_core::Host) -> Result<()> {
    let hook_cmd = r##"run-shell -b "case \"#{hook_session_name}\" in shelbi-*) tmux kill-session -t \"_#{hook_session_name}\" 2>/dev/null;; esac""##;
    let _ = shelbi_ssh::run(
        host,
        ["tmux", "set-hook", "-g", "session-closed[42]", hook_cmd],
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Shelbi-owned pane command builders.
//
// Single source of truth for what each pane runs. Both `ensure_dashboard`
// (first-time bootstrap) and `reload` (in-place respawn after a fresh
// binary install) format their `sh -c` strings through these — otherwise
// they would drift.

fn current_exe_string() -> Result<String> {
    Ok(std::env::current_exe()
        .map_err(Error::Io)?
        .to_string_lossy()
        .into_owned())
}

fn sidebar_cmd(shelbi_bin: &str, project_name: &str) -> String {
    format!(
        "{bin} __sidebar {proj}",
        bin = shelbi_agent::shell_escape(shelbi_bin),
        proj = shelbi_agent::shell_escape(project_name),
    )
}

// Tasks + review are real ratatui apps (`shelbi __tasks <p>`, `shelbi
// __review <p>`). Wrap each in a `while true` loop so an accidental crash
// or Ctrl-C respawns the TUI instead of leaving the stash pane empty —
// palette swap-pane assumes the pane id stays alive.
fn tasks_cmd(shelbi_bin: &str, project_name: &str) -> String {
    format!(
        "while true; do {bin} __tasks {proj}; sleep 1; done",
        bin = shelbi_agent::shell_escape(shelbi_bin),
        proj = shelbi_agent::shell_escape(project_name),
    )
}

fn review_cmd(shelbi_bin: &str, project_name: &str) -> String {
    format!(
        "while true; do {bin} __review {proj}; sleep 1; done",
        bin = shelbi_agent::shell_escape(shelbi_bin),
        proj = shelbi_agent::shell_escape(project_name),
    )
}

// Live worker/machine table — `shelbi worker list` probes each worker's
// tmux pane and prints the assigned task (if any), so remote workers
// show up alongside local ones with the same shape. Refresh every 5s;
// the SSH probe per remote worker keeps this cheap-but-not-free, hence
// the slower cadence than the kanban view.
fn machines_cmd(shelbi_bin: &str, project_name: &str) -> String {
    format!(
        "while true; do printf '\\033c'; echo 'workers · {label}'; echo; {bin} --project {proj} worker list 2>&1; sleep 5; done",
        bin = shelbi_agent::shell_escape(shelbi_bin),
        proj = shelbi_agent::shell_escape(project_name),
        label = project_name,
    )
}

// ---------------------------------------------------------------------------
// reload — respawn shelbi-owned panes in-place so a freshly installed
// binary takes effect without disturbing the orchestrator or workers.

/// Respawn the four long-lived shelbi-owned panes in-place so an updated
/// `shelbi` binary takes effect. Targets:
///
/// - `shelbi-<project>:dashboard.{left}` → `shelbi __sidebar <project>`
/// - stash `tasks` pane → tasks-view loop
/// - stash `review` pane → review-view loop
/// - stash `machines` pane → `shelbi worker list` loop
///
/// Out of scope: the orchestrator pane (claude re-shells out on each
/// CLI call) and worker panes (same). Those pick up the new binary
/// automatically the next time they invoke `shelbi`.
///
/// Idempotent: re-running incurs a visible flicker per pane but no
/// state loss — the panes' job is to render derived state from disk,
/// so a fresh process picks up where the old one was.
pub fn reload(project_name: &str) -> Result<ReloadReport> {
    let session = format!("shelbi-{project_name}");

    // Session must exist — there's nothing to reload if the user hasn't
    // booted the dashboard yet.
    if !local_session_exists(&session)? {
        return Err(Error::Other(format!(
            "session `{session}` not running; run `shelbi orchestrate` first"
        )));
    }

    let shelbi_bin = current_exe_string()?;
    let mut report = ReloadReport::default();

    // 1. Sidebar — pane id isn't stored at bootstrap; target positionally.
    //    `dashboard.{left}` resolves to the leftmost pane in the dashboard
    //    window, which is always the sidebar (the orchestrator's split
    //    landed on the right and view-swaps only touch dashboard.{right}).
    let sidebar_target = format!("{session}:dashboard.{{left}}");
    report.sidebar = respawn_pane(&sidebar_target, &sidebar_cmd(&shelbi_bin, project_name));

    // 2-4. Stash panes — pane ids are stored in session env at bootstrap.
    report.tasks = reload_stash_pane(&session, "tasks", &tasks_cmd(&shelbi_bin, project_name));
    report.review = reload_stash_pane(&session, "review", &review_cmd(&shelbi_bin, project_name));
    report.machines = reload_stash_pane(
        &session,
        "machines",
        &machines_cmd(&shelbi_bin, project_name),
    );

    Ok(report)
}

fn reload_stash_pane(session: &str, view: &str, cmd: &str) -> PaneReloadStatus {
    match read_pane_id(session, view) {
        Ok(Some(id)) => respawn_pane(&id, cmd),
        Ok(None) => PaneReloadStatus::Missing,
        Err(e) => PaneReloadStatus::Failed {
            target: format!("(env SHELBI_PANE_{view})"),
            reason: e.to_string(),
        },
    }
}

/// `tmux has-session -t <name>` — true if the session is alive on the
/// local tmux server. Reload always runs on the hub (matching the
/// `show_view` convention), so we don't route through `shelbi-ssh`.
fn local_session_exists(session: &str) -> Result<bool> {
    let out = std::process::Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .map_err(Error::Io)?;
    Ok(out.status.success())
}

/// Read `SHELBI_PANE_<view>` from the session's tmux environment.
/// Returns `None` if the variable was never set (older sessions
/// pre-dating the stash layout, or a partially-bootstrapped session).
fn read_pane_id(session: &str, view: &str) -> Result<Option<String>> {
    let key = format!("SHELBI_PANE_{view}");
    let out = std::process::Command::new("tmux")
        .args(["show-environment", "-t", session, &key])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim();
    // `-KEY` form means the variable is explicitly unset on this session.
    if line.starts_with('-') {
        return Ok(None);
    }
    let Some((_, value)) = line.split_once('=') else {
        return Ok(None);
    };
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value.to_string()))
    }
}

/// `tmux respawn-pane -k -t <target> sh -c <cmd>` — kill the running
/// process in the pane (`-k`) and start a fresh one. The pane's id is
/// preserved, so any swap-pane references stay valid.
fn respawn_pane(target: &str, cmd: &str) -> PaneReloadStatus {
    let out = std::process::Command::new("tmux")
        .args(["respawn-pane", "-k", "-t", target, "sh", "-c", cmd])
        .output();
    match out {
        Ok(o) if o.status.success() => PaneReloadStatus::Respawned {
            target: target.to_string(),
        },
        Ok(o) => PaneReloadStatus::Failed {
            target: target.to_string(),
            reason: String::from_utf8_lossy(&o.stderr).trim().to_string(),
        },
        Err(e) => PaneReloadStatus::Failed {
            target: target.to_string(),
            reason: e.to_string(),
        },
    }
}

#[cfg(test)]
mod pane_cmd_tests {
    use super::*;

    // These tests lock in the exact `sh -c` strings used for each shelbi-
    // owned pane. Both `ensure_dashboard` and `reload` route through the
    // same builders, so a regression here means the two paths could
    // disagree on what the pane runs.

    #[test]
    fn sidebar_cmd_is_invocation_of_internal_subcommand() {
        let out = sidebar_cmd("/usr/local/bin/shelbi", "myapp");
        assert_eq!(out, "/usr/local/bin/shelbi __sidebar myapp");
    }

    #[test]
    fn tasks_cmd_wraps_in_respawn_loop() {
        let out = tasks_cmd("/usr/local/bin/shelbi", "myapp");
        assert_eq!(
            out,
            "while true; do /usr/local/bin/shelbi __tasks myapp; sleep 1; done"
        );
    }

    #[test]
    fn review_cmd_wraps_in_respawn_loop() {
        let out = review_cmd("/usr/local/bin/shelbi", "myapp");
        assert_eq!(
            out,
            "while true; do /usr/local/bin/shelbi __review myapp; sleep 1; done"
        );
    }

    #[test]
    fn machines_cmd_calls_worker_list_on_a_loop() {
        let out = machines_cmd("/usr/local/bin/shelbi", "myapp");
        // sanity check: clears the screen each tick, runs `worker list`,
        // and threads --project through so the inner subcommand picks the
        // right project even though it's invoked through `sh -c`.
        assert!(out.contains("printf '\\033c'"));
        assert!(out.contains("/usr/local/bin/shelbi --project myapp worker list"));
        assert!(out.contains("sleep 5"));
    }

    #[test]
    fn cmd_builders_shell_escape_paths_with_spaces() {
        // A binary path with spaces (`/Users/jane doe/.cargo/bin/shelbi`)
        // would tear apart in `sh -c` without quoting.
        let out = sidebar_cmd("/Users/jane doe/.cargo/bin/shelbi", "myapp");
        assert_eq!(out, "'/Users/jane doe/.cargo/bin/shelbi' __sidebar myapp");
    }
}
